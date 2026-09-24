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
//!   ohne `bwrap` läuft für Modell-Aufrufe ausschließlich über
//!   [`ShellExecutor::run_command`] (Plan `recursive-cooking-lobster.md` Teil B1).
//!   Runde 6, Teil B: [`run_operator_command`]/[`OperatorCommand`] — `!`-Befehle
//!   der Nutzerin immer auf dem Host (ohne Freigabe, sudo bleibt abgelehnt,
//!   Audit `shell.operator_exec`)
//! - [`limits`] — [`ShellLimits`], [`ShellLimitsError`]: rlimits über festgepinntes `prlimit`
//!   und tmpfs-Größe (W1-03)
//! - [`host_permit_prompt`] — [`HostPermitPrompt`], [`HostPermitVariant`],
//!   [`HostPermitPromptSender`]/[`HostPermitPromptReceiver`]: der geteilte
//!   Fragekanal für Host-Profil-Permit-Anfragen zwischen `ShellExecutor` und
//!   einer anzeigenden Oberfläche (z. B. `harw-tui`); außerdem
//!   [`HostPermitHandles`] (gebündelter ServiceMap-Eintrag, Plan Teil B3) und
//!   [`SANDBOX_LEASE_WORKER_DEFINITION`] (Worker-Definition der
//!   `sandbox-lease`-Operation, Plan Teil B5)
//! - [`host_escalation`] — [`HostEscalation`], [`HostRequesterBook`],
//!   [`HostRequester`]: Host-Mode-Anfrage aus dem Orchestrator-Baum
//!   (`shell.exec` mit `request_host`, nur mit TUI-Freigabekanal; Runde 5,
//!   Teil N)
//! - [`latex`] — [`LatexToolProvider`]: typisiertes Werkzeug `latex.build`
//!   (festes `latexmk`-argv in derselben Bubblewrap-Sandbox, Runde 4 Teil E)
//! - [`sudo`] — [`SudoToolProvider`]: Werkzeug `host.sudo_exec` (ein Root-Befehl
//!   mit exaktem argv über festgepinntes `sudo`, freigegeben im eigenen
//!   TUI-Fenster; Fragekanal [`SudoPrompt`]/[`SudoAnswer`], Runde 5 Teil B)
//! - `capture` (intern) — streamende, gekappte Erfassung von stdout/stderr (W1-03)

#![forbid(unsafe_code)]

mod capture;
pub mod exec;
pub mod host_escalation;
pub mod host_permit_prompt;
pub mod latex;
pub mod limits;
pub mod sudo;

pub use exec::{
    BUILD_COMMAND_DEFAULT_TIMEOUT_SECS, DEFAULT_MAX_TIMEOUT_SECS, HOST_PERMIT_PROMPT_TIMEOUT,
    HOST_SESSION_LEASE_TTL, ShellExecError, ShellExecutor, ShellToolProvider,
};
// Runde 6, Teil B: Operator-Weg für `!`-Befehle der Nutzerin (immer Host).
pub use exec::{
    OPERATOR_DEFAULT_TIMEOUT_SECS, OperatorCommand, OperatorEnd, OperatorOutcome,
    operator_escalation_message, run_operator_command,
};
// Runde 5, Teil N: Host-Mode-Anfrage aus dem Orchestrator-Baum.
pub use host_escalation::{
    HOST_ESCALATION_DENIED_MSG, HOST_MODE_REQUIRES_TUI_MSG, HostEscalation, HostRequester,
    HostRequesterBook, SandboxDenial, classify_sandbox_denial,
};
pub use host_permit_prompt::{
    HostPermitHandles, HostPermitPrompt, HostPermitPromptReceiver, HostPermitPromptSender,
    HostPermitVariant, SANDBOX_LEASE_WORKER_DEFINITION, host_permit_prompt_channel,
};
pub use latex::{LATEX_BUILD_TOOL, LatexEngine, LatexToolProvider};
pub use limits::{ShellLimits, ShellLimitsError};
pub use sudo::{
    SUDO_EXEC_TOOL, SUDO_MAX_SECRET_BYTES, SUDO_PROMPT_TIMEOUT, SudoAnswer, SudoAuditRecord,
    SudoAuditSink, SudoAuthFailureHook, SudoPrompt, SudoPromptReceiver, SudoPromptSender,
    SudoSecret, SudoSecretError, SudoToolProvider, TracingSudoAudit, sudo_prompt_channel,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
