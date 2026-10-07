//! `harw-tool-gitread` — typisierte, rein lesende Git-Werkzeuge.
//!
//! Siehe `docs/planning/93-common-command-tools/README.md`.

#![forbid(unsafe_code)]

pub mod blame;
pub mod branch;
pub mod config;
pub mod diffcore;
pub mod fmtutil;
pub mod graph;
pub mod ignores;
pub mod index;
pub mod log;
pub mod object;
pub mod odb;
pub mod oid;
pub mod pathspec;
pub mod refs;
pub mod repo;
pub mod rev;
pub mod show;
pub mod snapshot;
pub mod status;
pub mod tree;

pub mod tools;

pub use tools::{GIT_TOOL_NAMES, GitReadToolProvider};

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tools_tests;
