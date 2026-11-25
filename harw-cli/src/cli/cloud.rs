//! CLI grammar for offline cloud workload policy inspection.

use std::path::PathBuf;

use clap::{Subcommand, ValueHint};

/// Actions under `harw cloud`.
#[derive(Debug, Subcommand)]
pub enum CloudAction {
    /// Validate or describe an explicit local policy; no live placement or dispatch.
    Policy {
        /// Offline operation to perform.
        #[command(subcommand)]
        action: CloudPolicyAction,
    },
}

/// Local owner-only policy operations.
#[derive(Debug, Subcommand)]
pub enum CloudPolicyAction {
    /// Validate the syntax and configured profile/workload combinations.
    Validate {
        /// Owner-private regular policy TOML file (for example, Unix mode 0600).
        #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
        file: PathBuf,
    },
    /// List the configured nodes, profiles and workload ceilings (not live nodes).
    Describe {
        /// Owner-private regular policy TOML file (for example, Unix mode 0600).
        #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
        file: PathBuf,
    },
}
