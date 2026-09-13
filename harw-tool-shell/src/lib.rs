//! `harw-tool-shell` — Shell-Exec-Tool für den Harwness Coding-Agent.
//!
//! Provides [`ShellToolProvider`] which registers the `shell.exec` function-tool.
//! Execution is sandbox-bound: a call is rejected unless [`harw_sandbox::Permission::ExecuteProcess`]
//! is present in the active [`harw_sandbox::SandboxSpec`]. The subprocess always
//! runs with its working directory set to the sandbox's canonical workspace root.
//!
//! # Security
//! This crate provides only the tool-surface for the model. Process isolation at
//! the OS level (bwrap, namespaces, seccomp) is the responsibility of `harw-sandbox`
//! and the deployment harness — see [`exec`] module documentation for details.
//!
//! # Modules
//! - [`exec`] — [`ShellToolProvider`], [`ShellExecutor`], [`ShellExecError`]

#![forbid(unsafe_code)]

pub mod exec;

pub use exec::{ShellExecError, ShellExecutor, ShellToolProvider};
