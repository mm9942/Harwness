//! `harw-tool-gitread` — typisierte, rein lesende Git-Werkzeuge.
//!
//! Siehe `docs/planning/93-common-command-tools/README.md`.

#![forbid(unsafe_code)]

pub mod config;
pub mod graph;
pub mod ignores;
pub mod index;
pub mod object;
pub mod odb;
pub mod oid;
pub mod pathspec;
pub mod refs;
pub mod repo;
pub mod rev;
pub mod tree;

#[cfg(test)]
mod test_support;
