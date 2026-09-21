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
//! - [`exec`] — [`ShellToolProvider`], [`ShellExecutor`], [`ShellExecError`],
//!   [`HOST_SESSION_LEASE_TTL`], [`HOST_PERMIT_PROMPT_TIMEOUT`]: Host-Ausführung
//!   ohne `bwrap` läuft ausschließlich über [`ShellExecutor::run_command`]
//!   (Plan `recursive-cooking-lobster.md` Teil B1)
//! - [`limits`] — [`ShellLimits`], [`ShellLimitsError`]: rlimits über festgepinntes `prlimit`
//!   und tmpfs-Größe (W1-03)
//! - [`host_permit_prompt`] — [`HostPermitPrompt`], [`HostPermitVariant`],
//!   [`HostPermitPromptSender`]/[`HostPermitPromptReceiver`]: der geteilte
//!   Fragekanal für Host-Profil-Permit-Anfragen zwischen `ShellExecutor` und
//!   einer anzeigenden Oberfläche (z. B. `harw-tui`); außerdem
//!   [`HostPermitHandles`] (gebündelter ServiceMap-Eintrag, Plan Teil B3) und
//!   [`SANDBOX_LEASE_WORKER_DEFINITION`] (Worker-Definition der
//!   `sandbox-lease`-Operation, Plan Teil B5)
//! - `capture` (intern) — streamende, gekappte Erfassung von stdout/stderr (W1-03)

#![forbid(unsafe_code)]

mod capture;
pub mod exec;
pub mod host_permit_prompt;
pub mod limits;

pub use exec::{
    HOST_PERMIT_PROMPT_TIMEOUT, HOST_SESSION_LEASE_TTL, ShellExecError, ShellExecutor,
    ShellToolProvider,
};
pub use host_permit_prompt::{
    HostPermitHandles, HostPermitPrompt, HostPermitPromptReceiver, HostPermitPromptSender,
    HostPermitVariant, SANDBOX_LEASE_WORKER_DEFINITION, host_permit_prompt_channel,
};
pub use limits::{ShellLimits, ShellLimitsError};
