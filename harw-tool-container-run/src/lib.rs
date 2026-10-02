//! `harw-tool-container-run`: the `container.*` agent tools.
//!
//! # Responsibility
//! The I/O layer on top of the pure policy core `harw-tool-container`:
//!
//! - [`engine`]: [`engine::ContainerEngine`] and [`engine::PodmanEngine`] run
//!   one validated [`harw_tool_container::ContainerPlan`], read back what the
//!   engine enforced while the container runs (inspect), kill it on a
//!   violation and bound output, wall time and cancellation.
//! - [`inspect`]: parses `podman inspect` JSON into
//!   [`harw_tool_container::InspectFacts`] (absent = `None` = unverifiable).
//! - [`tools`]: `container.images` and `container.run` and their provider.
//!
//! # Security contract
//! - Both tools require [`harw_authority::Permission::ManageContainers`]
//!   (checked before any argument is read).
//! - `container.run` always asks for approval (listed in the registry's
//!   always-ask set); a tier or an allow rule never skips it.
//! - The model names an image *alias*; digests, mounts, network, caps,
//!   limits and the engine path come from trusted config, not from the call.
//! - The read-back runs against the live container. A reported violation
//!   (`NotEnforced`) kills the container at once; properties the engine did
//!   not report are `Unverifiable` and are reported, not trusted. A container
//!   that exits before the first inspect cannot be verified; that is stated in
//!   the result, never silently "enforced".

#![forbid(unsafe_code)]

pub mod engine;
pub mod inspect;
pub mod tools;

pub use engine::{ContainerEngine, EngineError, EngineRun, PodmanEngine, RunOutput};
pub use tools::{
    CONTAINER_IMAGES_TOOL, CONTAINER_RUN_TOOL, ContainerToolConfig, ContainerToolProvider,
};
