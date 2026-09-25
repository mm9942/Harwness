//! `harw-agent-runner`: the standalone binary a compiled agent artifact is
//! embedded into.
//!
//! # Responsibility
//! `harw-agent-compiler` builds one agent binary as `runner ‖ artifact ‖
//! footer` ([`harw_agent_artifact::embed`]): the generic `harw-agent-runner`
//! executable with the compiled agent's [`harw_agent_artifact::Artifact`]
//! appended. Starting that binary:
//!
//! 1. loads and verifies the embedded artifact, fail-closed
//!    ([`verify::load_from_current_exe`]/[`verify::load_from_bytes`]) —
//!    `--verify`/`--manifest`/`--capabilities`/`--version` answer from this
//!    step alone and never reach a session;
//! 2. narrows the manifest's rights by the command line's rights flags
//!    (`--deny-tool`/`--no-network`/`--read-only`/`--full-access`/
//!    `--max-tokens`) and builds the [`harw_runtime::EmbeddedAgent`];
//! 3. picks an interface ([`choose_interface`]: `--interface`, else the
//!    manifest's default, else its first) and hands it a
//!    [`context::RunnerContext`] built around that agent — or, with
//!    `--child`, hands the context to [`child::run_child`] instead.
//!
//! # Two ways in
//! [`run_from_current_exe`] (what `main` calls) reads the running
//! executable's own file and extracts the footer-embedded artifact.
//! [`run_embedded`] instead takes the artifact's bytes directly (e.g.
//! `include_bytes!` in a natively generated wrapper crate that links this
//! library) — same verification, no footer indirection.
//!
//! # Modules
//! | Module | Owner (wave 3) |
//! |---|---|
//! | [`args`], [`context`], [`error`], [`capabilities`], [`verify`], this file | agent 1 |
//! | [`iface::cli`], [`iface::repl`] | agent 4 |
//! | [`iface::mcp`] | agent 5 |
//! | [`iface::http`] | agent 6 |
//! | [`iface::tui`] | agent 7 |
//! | [`child`] | agent 8 |

#![forbid(unsafe_code)]

pub mod args;
mod child;
mod child_protocol;
mod job_child_backend;
pub mod context;
pub mod error;

pub mod capabilities;
pub mod iface;
pub mod verify;

use std::process::ExitCode;
use std::sync::Arc;

use harw_agent_artifact::{Artifact, Bundle};
use harw_agent_dsl::ir_v2::{Binary, Interface};
use harw_runtime::EmbeddedAgent;
use harw_runtime::embedded::EffectiveRights;

use args::RunnerArgs;
use context::RunnerContext;
use error::RunnerError;

/// Runs against an artifact already extracted from its own bytes: a
/// natively generated wrapper's `include_bytes!(..)`, not the currently
/// running executable's footer.
///
/// # Description
/// Reads `std::env::args()` (the program name is skipped) exactly like
/// [`run_from_current_exe`]; the only difference is where the artifact
/// bytes come from.
#[must_use]
pub fn run_embedded(artifact: &'static [u8]) -> ExitCode {
    run(|| verify::load_from_bytes(artifact))
}

/// Runs against the artifact embedded (by [`harw_agent_artifact::embed`])
/// at the end of the currently running executable. This is what
/// `harw-agent-runner`'s [`main`] calls.
#[must_use]
pub fn run_from_current_exe() -> ExitCode {
    run(verify::load_from_current_exe)
}

/// Reports an error on stderr and returns its mapped exit code.
fn report_error(error: &RunnerError) -> ExitCode {
    eprintln!("harw-agent-runner: {error}");
    ExitCode::from(error.exit_code())
}

/// Prints a one-line manifest banner to stderr, unless `--json` was given
/// (a machine-reading caller of `--json` gets nothing but the JSON it
/// asked for on stdout).
fn print_banner(ir: &harw_agent_dsl::ir_v2::AgentIr, json: bool) {
    if json {
        return;
    }
    eprintln!(
        "harw-agent-runner: {} v{} ({})",
        ir.id, ir.version.0, ir.specialization
    );
}

/// Picks the interface to run, in the order the contract fixes: the
/// `--interface` flag, then the manifest's `[binary] default_interface`,
/// then the first entry of `[binary] interfaces` — checked against both the
/// manifest's allowed interfaces and the interfaces this runner build was
/// actually compiled with. A pure function so the precedence is unit
/// tested without an artifact.
///
/// # Errors
/// [`RunnerError::NoInterface`] if the manifest lists none and no flag was
/// given; [`RunnerError::UnknownInterface`] if the chosen interface is not
/// one `binary.interfaces` allows; [`RunnerError::NotCompiled`] if it is
/// allowed but this build lacks its feature.
pub fn choose_interface(
    flag: Option<Interface>,
    binary: &Binary,
    compiled: &[&str],
) -> Result<Interface, RunnerError> {
    let candidate = flag
        .or(Some(binary.default_interface))
        .or_else(|| binary.interfaces.first().copied())
        .ok_or(RunnerError::NoInterface)?;
    if !binary.interfaces.contains(&candidate) {
        return Err(RunnerError::UnknownInterface {
            requested: candidate.as_str().to_owned(),
            allowed: binary
                .interfaces
                .iter()
                .map(|interface| interface.as_str().to_owned())
                .collect(),
        });
    }
    if !compiled.contains(&candidate.as_str()) {
        return Err(RunnerError::NotCompiled {
            requested: candidate.as_str().to_owned(),
            compiled: compiled.iter().map(|feature| (*feature).to_owned()).collect(),
        });
    }
    Ok(candidate)
}

/// Runs one interface module for `ctx.args.interface`'s resolved choice.
///
/// Each arm is compiled only behind its feature; the `not(feature = ..)`
/// twin exists solely so every [`Interface`] variant has a match arm
/// regardless of which features this build enables — [`choose_interface`]
/// already turned "not compiled in" into [`RunnerError::NotCompiled`]
/// before this function is ever reached, so that twin is unreachable at
/// run time.
fn dispatch(interface: Interface, ctx: RunnerContext) -> ExitCode {
    match interface {
        Interface::Cli => {
            #[cfg(feature = "cli")]
            {
                iface::cli::run(ctx)
            }
            #[cfg(not(feature = "cli"))]
            {
                let _ = ctx;
                unreachable!("choose_interface rejects an interface this build lacks")
            }
        }
        Interface::Repl => {
            #[cfg(feature = "repl")]
            {
                iface::repl::run(ctx)
            }
            #[cfg(not(feature = "repl"))]
            {
                let _ = ctx;
                unreachable!("choose_interface rejects an interface this build lacks")
            }
        }
        Interface::Mcp => {
            #[cfg(feature = "mcp")]
            {
                iface::mcp::run(ctx)
            }
            #[cfg(not(feature = "mcp"))]
            {
                let _ = ctx;
                unreachable!("choose_interface rejects an interface this build lacks")
            }
        }
        Interface::Http => {
            #[cfg(feature = "http")]
            {
                iface::http::run(ctx)
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = ctx;
                unreachable!("choose_interface rejects an interface this build lacks")
            }
        }
        Interface::Tui => {
            #[cfg(feature = "tui")]
            {
                iface::tui::run(ctx)
            }
            #[cfg(not(feature = "tui"))]
            {
                let _ = ctx;
                unreachable!("choose_interface rejects an interface this build lacks")
            }
        }
    }
}

/// The shared body of [`run_embedded`] and [`run_from_current_exe`]: parse
/// args, answer the no-session commands, then load, verify, narrow and
/// dispatch.
fn run(load: impl FnOnce() -> Result<(Artifact, Bundle), RunnerError>) -> ExitCode {
    let args = match RunnerArgs::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => return report_error(&error),
    };

    if args.version {
        return verify::run_version();
    }
    if args.capabilities {
        return match capabilities::render() {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => report_error(&RunnerError::from(error)),
        };
    }
    if args.verify {
        return verify::run_verify(load, args.json);
    }
    if args.manifest {
        return match verify::run_manifest(load, args.json) {
            Ok(code) => code,
            Err(error) => report_error(&error),
        };
    }

    let (artifact, bundle) = match load() {
        Ok(pair) => pair,
        Err(error) => return report_error(&error),
    };
    let agent = match EmbeddedAgent::from_bundle(bundle.clone(), &artifact) {
        Ok(agent) => agent,
        Err(error) => return report_error(&RunnerError::Runtime(error)),
    };
    let rights = EffectiveRights::from_manifest(&agent.root_ir().permissions)
        .narrowed_by(&args.flags);
    let agent = Arc::new(agent.with_rights(rights));

    print_banner(agent.root_ir(), args.json);

    if let Some(child_id) = args.child.clone() {
        let ctx = RunnerContext {
            bundle,
            agent,
            args,
        };
        return child::run_child(ctx, &child_id);
    }

    let interface = match choose_interface(
        args.interface,
        &agent.root_ir().binary,
        &capabilities::compiled_interfaces(),
    ) {
        Ok(interface) => interface,
        Err(error) => return report_error(&error),
    };
    let ctx = RunnerContext {
        bundle,
        agent,
        args,
    };
    dispatch(interface, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_agent_dsl::ir_v2::{Binary, ChildExecution, Interface};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn binary(interfaces: &[Interface], default_interface: Interface) -> Binary {
        Binary {
            name: "demo".to_owned(),
            interfaces: interfaces.to_vec(),
            default_interface,
            child_execution: ChildExecution::Job,
        }
    }

    #[test]
    fn test_flag_wins_over_manifest_default() -> TestResult {
        let manifest = binary(&[Interface::Cli, Interface::Mcp], Interface::Cli);
        let chosen = choose_interface(Some(Interface::Mcp), &manifest, &["cli", "mcp"])?;
        assert_eq!(chosen, Interface::Mcp);
        Ok(())
    }

    #[test]
    fn test_falls_back_to_manifest_default_interface() -> TestResult {
        let manifest = binary(&[Interface::Cli, Interface::Mcp], Interface::Mcp);
        let chosen = choose_interface(None, &manifest, &["cli", "mcp"])?;
        assert_eq!(chosen, Interface::Mcp);
        Ok(())
    }

    #[test]
    fn test_falls_back_to_first_interface_without_a_flag_or_default() -> TestResult {
        // `Binary::default_interface` is not `Option` in practice, but the
        // precedence chain still falls through to the first entry if a
        // caller ever constructs one outside that invariant.
        let manifest = binary(&[Interface::Repl, Interface::Cli], Interface::Repl);
        let chosen = choose_interface(None, &manifest, &["repl", "cli"])?;
        assert_eq!(chosen, Interface::Repl);
        Ok(())
    }

    #[test]
    fn test_rejects_an_interface_the_manifest_does_not_allow() {
        let manifest = binary(&[Interface::Cli], Interface::Cli);
        let error = choose_interface(Some(Interface::Http), &manifest, &["cli", "http"]);
        assert!(matches!(error, Err(RunnerError::UnknownInterface { .. })));
    }

    #[test]
    fn test_rejects_an_interface_this_build_lacks() {
        let manifest = binary(&[Interface::Cli, Interface::Http], Interface::Cli);
        let error = choose_interface(Some(Interface::Http), &manifest, &["cli"]);
        assert!(matches!(error, Err(RunnerError::NotCompiled { .. })));
    }

    #[test]
    fn test_no_interface_display_names_the_problem() {
        let error = RunnerError::NoInterface;
        assert!(error.to_string().contains("no interface"));
    }
}
