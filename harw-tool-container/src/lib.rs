//! `harw-tool-container`: policy core of the container tools.
//!
//! # Responsibility
//! Turn a small, validated request into one hardened `podman run` argument
//! vector, and judge afterwards whether the engine really enforced it. The
//! crate performs **no I/O and starts no process**; it has **no
//! dependencies**. It is the pure layer of the `container.*` tool family
//! proposed in `docs/research/container-and-cli-tools.md`, built the way
//! `harw-tool-tunnel` builds its policy core: the dangerous part is a plain
//! function that can be tested exhaustively without an engine.
//!
//! # Why a typed builder instead of a shell string
//! The sandbox withholds `/run`, so a model cannot reach a container socket
//! from `shell.exec`; the only alternative today is host-mode escalation.
//! A typed request that cannot express a mount, a network, a capability or a
//! privileged flag removes the need for that escalation.
//!
//! # Contract
//! - Images are digest-pinned ([`ImageRef`]); the model names an alias from
//!   an [`ImageCatalog`], never a reference.
//! - A [`Profile`] fixes workspace access, network (always `none`), memory,
//!   process count and the timeout ceiling.
//! - [`ContainerPlan::build`] is the only way to an argument vector. Every
//!   value is validated so that no value can add or reorder an engine flag
//!   (`--` precedes the image; separators are rejected in paths and names).
//! - There is no `run`. The lifecycle is `create`, inspect, verify, then
//!   `start` ([`Stage`]); a plan that fails verification is removed without
//!   ever having run, so the workload never executes under a weaker policy.
//! - The approval text shows the program and a SHA-256 of the command, never
//!   its arguments, which may carry credentials.
//! - [`engine_environment`] builds the engine process environment from an
//!   allowlist; variables that redirect an engine are never passed on.
//! - [`readback::verify`] compares the plan with the engine's inspect facts;
//!   a fact that was not reported is `Unverifiable`, never `Enforced`.
//!
//! # Not covered here
//! Running the engine, parsing inspect JSON, pulling images, signature
//! policy, Docker (flags such as `--userns=keep-id` are Podman-only),
//! registration in the capability catalog, and the permission check. Those
//! belong to the tool layer and to the shared registration files.
//!
//! # Concurrency
//! All types are plain values and `Send + Sync`.

#![forbid(unsafe_code)]

mod digest;
pub mod env;
pub mod error;
pub mod image;
pub mod mount;
pub mod plan;
pub mod profile;
pub mod readback;
mod validate;

#[cfg(test)]
mod test_support;

pub use env::{DEFAULT_ENV_ALLOW, REDIRECTING_VARIABLES, engine_environment};
pub use error::ContainerPolicyError;
pub use image::{ImageCatalog, ImageRef};
pub use mount::Mount;
pub use plan::{CACHE_DST, ContainerPlan, Expected, RunConfig, RunRequest, Stage, WORKSPACE_DST};
pub use profile::{Limits, Profile};
pub use readback::{Dimension, Enforcement, InspectFacts, Readback, verify};
