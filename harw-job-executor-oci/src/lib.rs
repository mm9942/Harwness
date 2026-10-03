//! OCI container executor for the job runtime.
//!
//! One attempt is one container, created from a digest-pinned image and
//! configured fail-closed (no capabilities, no new privileges, read-only
//! root, no network unless the profile allows it, no host mounts). After
//! `create` the applied configuration is read back with `inspect` and mapped
//! to a per-dimension [`harw_job_core::SandboxReport`]; a
//! `SandboxRequirement::Required` that the read-back does not satisfy fails
//! **before** the container is started, so no body ran.
//!
//! The engine is reached over its Unix socket with a minimal blocking
//! HTTP/1.1 client (no async runtime needed by the synchronous
//! `Executor::start`). The socket's owner is checked with `SO_PEERCRED`;
//! a rootful socket needs an explicit [`PeerPolicy::Uid`].
//!
//! Every action targets the engine-assigned container id, never a name, and
//! recovery re-verifies labels, creation time and image before it reports
//! a container as alive.

#![forbid(unsafe_code)]

#[cfg(unix)]
mod config;
#[cfg(unix)]
mod enforcement;
#[cfg(unix)]
mod engine;
#[cfg(unix)]
mod error;
#[cfg(unix)]
mod executor;
#[cfg(unix)]
mod gc;
#[cfg(unix)]
mod http;
#[cfg(unix)]
mod offer;
#[cfg(unix)]
mod pool;

#[cfg(all(test, unix))]
mod test_support;
#[cfg(all(test, unix))]
mod tests;

#[cfg(unix)]
pub use config::{OciConfig, PeerPolicy};
#[cfg(unix)]
pub use enforcement::{Requested, report_from_inspect};
#[cfg(unix)]
pub use engine::InspectDoc;
#[cfg(unix)]
pub use error::OciError;
#[cfg(unix)]
pub use executor::OciExecutor;
#[cfg(unix)]
pub use gc::{AttemptView, GcPolicy, GcReport, Listed, Reason, Removal, plan_gc};
#[cfg(unix)]
pub use pool::{PoolKey, Slot, WarmPool};
