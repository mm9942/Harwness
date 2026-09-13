//! Dependency-light command-shell primitives for the Harwness terminal UI.
//!
//! This crate deliberately owns only the interaction boundary: classifying an
//! input line, parsing slash-command tokens, finding a command specification,
//! and centrally enforcing permission, surface, and shell-capability gates.
//! Session, work, and catalog operations remain the responsibility of the
//! runtime crates wired in by a future TUI renderer.

#![forbid(unsafe_code)]

pub mod app;
pub mod approval;
pub(crate) mod chat_scroll;
pub(crate) mod input_history;
mod command;
pub(crate) mod command_exec;
pub(crate) mod command_popup;
mod error;
pub(crate) mod events;
pub(crate) mod frame_requester;
pub mod gateway;
pub(crate) mod history_cell;
mod input;
pub(crate) mod input_editor;
pub(crate) mod input_reader;
mod registry;
pub(crate) mod sanitize;
pub mod session_controller;
pub mod setup;
pub(crate) mod spinner;
pub(crate) mod streaming;
pub(crate) mod style;
pub mod tools_command;
pub(crate) mod tui_event;

pub use app::{ChatApp, TuiError, run_chat_tui};
pub use approval::{
    ApprovalDriver, ApprovalDriverError, ApprovalPrompt, ApprovalScope, ChildTurnDriver,
    ResumeStage, TuiApprovalHandler,
};
pub use command::{
    CommandDomain, CommandName, CommandScope, CommandSpec, OutputSurface, PermissionTier,
};
pub use error::{CommandError, CommandResult};
pub use gateway::{ChatGateway, LocalGateway};
pub use input::{Invocation, classify_input};
pub use registry::{
    CapabilitySet, CommandAction, CommandRegistry, DispatchContext, InvocationSurface,
    ShellCapability,
};
pub use setup::{SetupApp, SetupOutcome, SetupStage, run_setup};
