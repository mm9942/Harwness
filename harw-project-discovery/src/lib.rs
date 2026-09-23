//! `harw-project-discovery` — Marker-based project-root detection and
//! doc-cascade loading for the Harwness coding agent.
//!
//! Mirrors Codex' `project_root_markers` + `AGENTS.md` cascade pattern.
//! Exposes both a synchronous discovery API and a `ContextProvider`
//! implementation that emits ContextFragments per doc file found.
//!
//! # Module Layout
//!
//! - [`discovery`] — synchronous walk-up root detection and doc-file loading.
//! - [`provider`] — async `ContextProvider` implementation wrapping a
//!   [`discovery::ProjectContext`].
//!
//! # Key Types
//!
//! - [`DiscoveryConfig`] — controls which markers and doc filenames to scan.
//! - [`ProjectContext`] — the result of a discovery run (root + docs).
//! - [`ProjectContextProvider`] — emits [`harw_extension_api::ContextFragment`]s
//!   for the harw agent turn loop.
//!
//! # Concurrency
//!
//! `discovery::discover_project` is fully synchronous and may be called from
//! any thread. `ProjectContextProvider` is `Send + Sync`.
//!
//! # Examples
//!
//! ```rust,no_run
//! use std::path::Path;
//! use harw_project_discovery::{discover_project, DiscoveryConfig};
//!
//! let cfg = DiscoveryConfig::default();
//! let ctx = discover_project(Path::new("."), &cfg).expect("discovery failed");
//! println!("project root: {}", ctx.project_root.display());
//! for doc in &ctx.docs {
//!     println!("  doc: {}", doc.path.display());
//! }
//! ```
#![forbid(unsafe_code)]

pub mod discovery;
pub mod provider;

pub use discovery::{
    DiscoveredDoc, DiscoveryConfig, DiscoveryError, ProjectContext, discover_project,
};
pub use provider::ProjectContextProvider;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
