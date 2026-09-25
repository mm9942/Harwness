//! `iface::tui` — the mini-TUI interface: the normal harw TUI fixed to the
//! one agent embedded in this binary.
//!
//! # Responsibility
//! Builds the `EntryKind::CompiledAgent` [`RuntimeSpec`] for this run (cwd,
//! a local principal, and `spec.embedded` set to [`RunnerContext::agent`])
//! and the [`harw_tui::fixed_agent::FixedAgentOptions`] restriction policy
//! (title, allowed models) from the embedded manifest, then hands both to
//! [`harw_tui::fixed_agent::run_fixed_agent`] — the one seam
//! `harw-tui/src/fixed_agent.rs` exposes for this. This module never talks
//! to `harw_tui::run_tui`/`ChatApp` directly and never re-implements any
//! part of the renderer or command dispatch.
//!
//! # Gated by the `tui` feature
//! Mirrors `iface::cli`/`iface::repl`/`iface::mcp`/`iface::http`: only
//! compiled in when this runner build has the `tui` feature (see
//! `Cargo.toml`'s `default` feature list and `RunnerError::NotCompiled`,
//! which is what a manifest naming `tui` gets from a runner built without
//! it).

use std::process::ExitCode;
use std::sync::Arc;

use harw_agent_dsl::ir_v2::ChildExecution;
use harw_runtime::{ChildBackendHandle, EntryKind, RuntimeSpec};
use harw_tui::fixed_agent::{
    FixedAgentOptions, allowed_models_from_ir, fixed_agent_title, run_fixed_agent,
};
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};

use crate::context::RunnerContext;
use crate::error::EXIT_SOFTWARE;

/// Runs the mini-TUI for `ctx`'s embedded agent.
///
/// # Description
/// 1. Resolves `cwd` (`std::env::current_dir`) and builds a local
///    [`Principal`] the same way the interactive `harw` TUI does
///    (`PrincipalKind::Human`, [`IngressSurface::Tui`],
///    [`PermissionTier::Operator`]) — a compiled agent's mini-TUI is still a
///    human sitting at a terminal, only fixed to one agent rather than a
///    configured UIA.
/// 2. Builds an `EntryKind::CompiledAgent` [`RuntimeSpec`] with
///    `embedded: Some(Arc::clone(&ctx.agent))` and no mode/model/effort/
///    approval override — the embedded manifest is the only source of
///    those for this run.
/// 3. Builds [`FixedAgentOptions`] from `ctx.root_ir()`: `title` is the
///    manifest's display name (`name`, falling back to `specialization`
///    when the manifest names none), `allow_model_switch` is `false` (a
///    compiled agent pins its manifest's model; see the module fixed_agent
///    doc for the follow-up needed to allow a restricted switch instead of
///    none), and `allowed_models` is the manifest's model plus its
///    fallbacks ([`allowed_models_from_ir`]) — carried through even though
///    `allow_model_switch` is `false` today, so flipping that flag later is
///    the only change this call site needs.
/// 4. Calls [`run_fixed_agent`] and maps its `Result` to an [`ExitCode`].
///
/// # Arguments
/// - `ctx`: the runner context built from the verified embedded artifact
///   (`crate::context::RunnerContext`).
///
/// # Returns
/// `ExitCode::SUCCESS` on a clean TUI exit.
///
/// # Errors
/// Prints a one-line message to stderr and returns
/// `ExitCode::from(EXIT_SOFTWARE)` when the working directory cannot be
/// read or [`run_fixed_agent`] fails (assembly or terminal/turn-loop
/// failure) — the same `EX_SOFTWARE` mapping `RunnerError::Runtime` and
/// `RunnerError::Sdk` use elsewhere in this crate.
///
/// # Concurrency
/// Blocks the calling thread until the TUI exits, exactly like
/// [`run_fixed_agent`].
#[must_use]
pub fn run(ctx: RunnerContext) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("harw-agent-runner: tui: could not read the working directory: {error}");
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };

    let root_ir = ctx.root_ir();
    let title = root_ir
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| root_ir.specialization.clone());
    // A compiled agent's fixed model comes from its own manifest, resolved
    // by the same runtime that builds `spec.embedded`; `/model switch` stays
    // disabled rather than restricted-but-open until the `harw-tui` hook
    // documented in `fixed_agent.rs` lands (see that module's doc comment
    // for the exact spot). `allowed_models` is still computed so that flag
    // flip is this call site's only future change.
    let opts = FixedAgentOptions {
        title,
        allow_model_switch: false,
        allowed_models: allowed_models_from_ir(root_ir),
    };
    tracing::debug!(
        title = %fixed_agent_title(&opts, ctx.agent.digest()),
        allowed_models = opts.allowed_models.len(),
        "agent-runner.tui.starting"
    );

    let home = match harw_home::home_dir() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("harw-agent-runner: tui: could not resolve the root space: {error}");
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };
    // Wie `RunnerContext::builder`: `[binary].child_execution = "job"` (der
    // Standard) lässt jedes Kind als eigenen Job-Prozess laufen.
    let child_backend = if root_ir.binary.child_execution == ChildExecution::Job {
        match ctx.job_child_backend() {
            Ok(backend) => Some(ChildBackendHandle(backend)),
            Err(error) => {
                eprintln!("harw-agent-runner: tui: {error}");
                return ExitCode::from(EXIT_SOFTWARE);
            }
        }
    } else {
        None
    };
    let spec = RuntimeSpec {
        entry: EntryKind::CompiledAgent,
        home,
        cwd,
        principal: local_principal(),
        mode_override: None,
        active_agent: None,
        reasoning_effort: None,
        approval_override: None,
        model_override: None,
        embedded: Some(Arc::clone(&ctx.agent)),
        child_backend,
    };

    match run_fixed_agent(spec, opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("harw-agent-runner: tui: {error}");
            ExitCode::from(EXIT_SOFTWARE)
        }
    }
}

/// Builds the local, trusted [`Principal`] for a mini-TUI run.
///
/// # Description
/// Same construction the interactive `harw` TUI uses for a local caller
/// (`harw-cli/src/runtime_entry.rs::local_principal`), restated here rather
/// than imported: `harw-cli` is a separate composition root this crate does
/// not depend on, and the identity string does not need to be the
/// kernel-witnessed uid `harw-cli` uses (a compiled agent has no
/// multi-account host session to distinguish) — the login name is enough to
/// label the principal without adding a `rustix` dependency to this crate.
fn local_principal() -> Principal {
    let id = std::env::var("USER")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "local".to_owned());
    Principal::trusted_ingress(
        PrincipalKind::Human,
        id,
        IngressSurface::Tui,
        PermissionTier::Operator,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_principal_is_a_human_tui_operator() {
        let principal = local_principal();
        assert_eq!(principal.surface(), IngressSurface::Tui);
        assert_eq!(principal.tier(), PermissionTier::Operator);
    }
}
