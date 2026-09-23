//! Shell completion generation and managed installation for harw binaries.
//!
//! Spec source: `harw-completions-CODING-DESIGN.md` §1.
//!
//! # Responsibility
//! - [`shell`]: script generation (via `clap_complete`), ownership markers,
//!   clap signature detection and `$SHELL` detection.
//! - [`locations`]: per-shell canonical/legacy file locations, rc blocks and
//!   cache directories, resolved from an injected [`HomeEnv`].
//! - [`install`](mod@install): idempotent install/uninstall that sweeps older harw-managed
//!   installations.
//! - `args` (feature `clap-args`): reusable clap arguments and
//!   `run_completions` for binaries embedding a `completions` subcommand.
//!
//! # Concurrency
//! All functions are synchronous; types are plain data and `Send + Sync`.
//!
//! # Errors
//! Every fallible function returns [`CompletionResult`] with [`CompletionError`].
//!
//! # Examples
//! ```no_run
//! use harw_completions::{Shell, generate_script};
//!
//! let mut cmd = clap::Command::new("demo").version("1.0.0");
//! let script = generate_script(&mut cmd, "demo", Shell::Zsh);
//! assert!(!script.is_empty());
//! ```

#[cfg(feature = "clap-args")]
pub mod args;
pub mod error;
pub mod install;
pub mod locations;
pub mod shell;

#[cfg(feature = "clap-args")]
pub use args::{CompletionsArgs, CompletionsSubcommand, run_completions};
pub use clap_complete::Shell;
pub use error::{CompletionError, CompletionResult};
pub use install::{InstallOptions, InstallReport, SkippedPath, install, uninstall};
pub use locations::{BlockPlacement, HomeEnv, Locations, RcBlock, resolve_locations};
pub use shell::{
    MANAGED_MARKER_PREFIX, block_begin, block_end, detect_shell, generate_script,
    has_clap_signature, is_managed_script, is_ours, marker_line,
};
