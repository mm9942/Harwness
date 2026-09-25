//! The built-in interfaces, one module per `[binary] interfaces` label.
//!
//! # Description
//! Each module is written by its own agent (wave 3) and exposes exactly
//! `pub fn run(ctx: crate::context::RunnerContext) -> std::process::ExitCode`,
//! gated by the matching cargo feature so a narrowed native build can drop
//! the interfaces an agent's manifest never selects. This module only
//! declares them; [`crate::choose_interface`] and [`crate::run`] pick and
//! call the one the manifest and `--interface` select.

/// Terminal approval handler shared by `cli` and `repl` (the two
/// interfaces that run at an actual terminal); see its own module docs.
#[cfg(any(feature = "cli", feature = "repl"))]
mod approval;

/// One-shot command line: reads `RunnerArgs::prompt`, runs a single turn,
/// prints the report.
#[cfg(feature = "cli")]
pub mod cli;

/// Interactive read-eval-print loop in the terminal.
#[cfg(feature = "repl")]
pub mod repl;

/// Model Context Protocol server (stdio or `--listen`).
#[cfg(feature = "mcp")]
pub mod mcp;

/// HTTP/JSON API (`--listen`).
#[cfg(feature = "http")]
pub mod http;

/// Mini terminal UI.
#[cfg(feature = "tui")]
pub mod tui;
