//! `harw-tool-gitread` — typisierte, rein lesende Git-Werkzeuge.
//!
//! Siehe `docs/planning/93-common-command-tools/README.md`.

#![forbid(unsafe_code)]

pub mod object;
pub mod odb;
pub mod oid;
pub mod refs;
pub mod repo;

#[cfg(test)]
mod test_support;
