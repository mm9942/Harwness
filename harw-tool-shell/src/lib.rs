//! `harw-tool-shell` — Shell-Exec-Tool für den Harwness Coding-Agent.
//!
//! Provides [`ShellToolProvider`] which registers the `shell.exec` function-tool.
//! Execution is sandbox-bound: a call is rejected unless [`harw_authority::Permission::ExecuteProcess`]
//! is present in the active [`harw_authority::SandboxSpec`]. The subprocess always
//! runs with its working directory set to the sandbox's canonical workspace root.
//!
//! # Security
//! This crate provides only the tool-surface for the model. Process isolation at
//! the OS level (bwrap, namespaces, seccomp) is the responsibility of `harw-sandbox`
//! and the deployment harness — see [`exec`] module documentation for details.
//!
//! # Modules
//! - [`exec`] — [`ShellToolProvider`], [`ShellExecutor`], [`ShellExecError`]
//! - [`limits`] — [`ShellLimits`], [`ShellLimitsError`]: rlimits über festgepinntes `prlimit`
//!   und tmpfs-Größe (W1-03)
//! - [`host_permit_prompt`] — [`HostPermitPrompt`], [`HostPermitVariant`],
//!   [`HostPermitPromptSender`]/[`HostPermitPromptReceiver`]: der geteilte
//!   Fragekanal für Host-Profil-Permit-Anfragen zwischen `ShellExecutor` und
//!   einer anzeigenden Oberfläche (z. B. `harw-tui`)
//! - `capture` (intern) — streamende, gekappte Erfassung von stdout/stderr (W1-03)

#![forbid(unsafe_code)]

mod capture;
pub mod exec;
pub mod host_permit_prompt;
pub mod limits;

pub use exec::{ShellExecError, ShellExecutor, ShellToolProvider};
pub use host_permit_prompt::{
    HostPermitPrompt, HostPermitPromptReceiver, HostPermitPromptSender, HostPermitVariant,
    host_permit_prompt_channel,
};
pub use limits::{ShellLimits, ShellLimitsError};
