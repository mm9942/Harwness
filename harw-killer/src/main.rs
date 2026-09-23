//! `killer` binary: thin entry point over [`harw_killer::run_cli`].
//!
//! All selection, authorization, termination and reporting lives in the
//! `harw_killer` library; this binary only forwards its arguments and uses the
//! standalone helper re-execution (`killer --helper ...`).
//! # Examples
//! ```no_run
//! std::process::Command::new("killer").args(["-p", "cargo", "--dry-run"]).status()?;
//! # Ok::<(), std::io::Error>(())
//! ```
#![forbid(unsafe_code)]

use std::process::ExitCode;

// Forward process arguments; exit semantics are documented on `run_cli`.
fn main() -> ExitCode {
    harw_killer::run_cli(std::env::args_os(), harw_killer::HelperInvocation::Standalone)
}
