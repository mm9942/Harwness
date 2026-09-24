//! Dependency-light command-shell primitives for the Harwness terminal UI.
//!
//! This crate deliberately owns only the interaction boundary: classifying an
//! input line, parsing slash-command tokens, finding a command specification,
//! and centrally enforcing permission, surface, and shell-capability gates.
//! Session, work, and catalog operations remain the responsibility of the
//! runtime crates wired in by a future TUI renderer.

#![forbid(unsafe_code)]

pub(crate) mod agent_monitor;
pub(crate) mod agent_tree;
// Runde 5, Teil I: Live-Werte der Agentenbaum-Ansicht `/agent`.
pub(crate) mod agent_tree_live;
pub mod app;
pub mod approval;
pub mod approval_dialog;
// Runde 5, Teil F: Auswahlfenster für `ask_user`.
pub(crate) mod ask_user_dialog;
pub(crate) mod chat_scroll;
// Runde 5, Teil I: Live-Stream der Kind-Agenten im Verlauf.
pub(crate) mod child_stream;
pub(crate) mod choice_dialog;
pub mod clipboard;
mod command;
pub(crate) mod command_catalog;
pub(crate) mod command_data;
pub(crate) mod command_exec;
pub(crate) mod command_popup;
mod error;
pub(crate) mod events;
pub(crate) mod explorer_panel;
pub mod export;
pub(crate) mod frame_requester;
pub mod gateway;
pub(crate) mod help_overlay;
pub(crate) mod history_cell;
pub mod host_permit_dialog;
mod input;
pub(crate) mod input_editor;
pub(crate) mod input_history;
pub(crate) mod input_reader;
pub(crate) mod kanban_board;
pub(crate) mod keybindings;
pub(crate) mod knowledge_view;
pub(crate) mod local_commands;
pub(crate) mod markdown;
pub(crate) mod matrix_view;
pub(crate) mod mention;
pub(crate) mod mention_popup;
pub(crate) mod mode_picker;
pub mod model_picker;
pub(crate) mod model_roles_view;
pub(crate) mod model_switch_picker;
pub(crate) mod overlay_view;
pub(crate) mod panes;
// Runde 5, Teil E: Auto-Modus-Vermerke und Lern-Angebot (`pub`, weil
// `approval_dialog::ApprovalChoice` die Typen trägt).
pub mod permissions_view;
// Runde 5, Teil F: Plan-Freigabe (`plan.exit`), Plan-Vorschlag (`plan.enter`), `/plan …`.
pub(crate) mod plan_dialog;
// Runde 5, Teil P: Goal-Marke der Statuszeile und Goal-/Schritt-Verlaufszeilen.
pub(crate) mod goal_marker;
mod registry;
pub mod relative_time;
pub(crate) mod runtime_commands;
pub(crate) mod runtime_root;
pub(crate) mod sanitize;
pub mod session_controller;
// Runde 5, Teil H: leere Sitzungen nicht persistieren, Sidecars aufräumen.
pub(crate) mod session_lifecycle;
pub mod session_picker;
pub mod setup;
pub(crate) mod spinner;
pub(crate) mod status_line;
pub(crate) mod style;
pub(crate) mod sudo_dialog;
pub mod tools_command;
pub(crate) mod tui_event;
pub(crate) mod workbench_pane;

pub use app::{ChatApp, TuiError};
pub use approval::{
    ApprovalDriver, ApprovalDriverError, ApprovalPrompt, ApprovalScope, ChildTurnDriver,
    ResumeStage, TuiApprovalHandler,
};
pub use approval_dialog::{ApprovalChoice, ApprovalDialog, ApprovalDialogRequest, DialogAction};
pub use clipboard::{ClipboardTarget, copy_or_sequence, copy_to_clipboard, osc52_sequence};
pub use command::{
    CommandDomain, CommandName, CommandOrigin, CommandScope, CommandSpec, OutputSurface,
    PermissionTier, SubcommandHint,
};
pub use error::{CommandError, CommandResult};
pub use export::{
    ExportEntry, ExportError, ExportMeta, ExportOptions, default_export_path, render_markdown,
    write_export,
};
pub use gateway::{ChatGateway, LocalGateway};
pub use input::{Invocation, classify_input};
pub use model_picker::{ModelPickerOutcome, ModelPickerProvider, run_model_picker};
pub use registry::{
    CapabilitySet, CommandAction, CommandRegistry, DispatchContext, InvocationSurface,
    ShellCapability,
};
pub use relative_time::relative_time;
pub use runtime_root::{TuiAssemblyFactory, TuiResume, TuiRunOptions, TuiSessionWiring, run_tui};
pub use session_picker::{PickerAction, SessionEntry, SessionPicker};
pub use setup::{SetupApp, SetupOutcome, SetupStage, run_setup};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
