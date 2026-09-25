//! `repl`: the interactive, multi-turn interface (feature `repl`).
//!
//! # Description
//! One [`harwness_sdk::Session`] for the whole process: each line read from
//! stdin is a new turn on the *same* session, so the agent keeps the
//! conversation's history ([`harwness_sdk::Session::history`]) across turns
//! — the difference from `iface::cli`, which is one turn and exits.
//!
//! Uses only a plain stdin line loop (`std::io::stdin().read_line`), not
//! `rustyline`/`reedline`: neither is in `Cargo.lock` yet (no workspace
//! crate depends on either), and pulling one in for a wave-3B interface
//! that this agent does not use interactively in CI is not worth a new
//! dependency. A later wave can add real line editing and history without
//! changing this module's shape.
//!
//! # Commands
//! - `/exit`: ends the REPL (also end-of-input, e.g. piped stdin or Ctrl+D).
//! - `/manifest`: prints the embedded agent's permissions
//!   ([`harw_agent_dsl::ir_v2::AgentIr::permissions`]) as JSON; not a turn.
//! - `/cancel`: cancels the turn currently running. Since this loop reads
//!   one line at a time and only starts the next turn after the previous
//!   `send` returns, `/cancel` typed at the prompt (rather than while a
//!   turn is in flight) has nothing to cancel and says so. Ctrl+C
//!   (`SIGINT`) is wired to the same [`harwness_sdk::CancelHandle`] so it
//!   *does* reach a turn that is actually running.
//!
//! Approval is the same [`super::approval::TerminalApproval`] `iface::cli`
//! uses (shared, not reimplemented), remembering "immer" answers for the
//! rest of the session across every turn, not just the current one.
//!
//! Each turn's outcome is reported on stderr after it ends (dim, like tool
//! calls); the REPL itself always exits `0` on a normal `/exit` or
//! end-of-input — a failed or cancelled *turn* is not a failed *session*,
//! unlike `iface::cli`'s one-shot exit code.

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;

use harwness_sdk::{SdkEvent, Session, ToolOutput, TurnStatus};

use super::approval::{TerminalApproval, build_harwness};
use crate::context::RunnerContext;

/// Entry point for `iface::Interface` (feature `repl`).
pub fn run(ctx: RunnerContext) -> ExitCode {
    let approvals = Arc::new(TerminalApproval::new(ctx.args.flags.full_access));
    let harwness = match build_harwness(&ctx, approvals) {
        Ok(harwness) => harwness,
        Err(error) => {
            eprintln!("harw-agent-runner: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("harw-agent-runner: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(repl_loop(&ctx, harwness)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("harw-agent-runner: {error}");
            ExitCode::FAILURE
        }
    }
}

/// One line of terminal input, already trimmed.
enum Line {
    /// End of input (Ctrl+D / a closed pipe): the loop ends like `/exit`.
    Eof,
    /// A blank line: prompted again, nothing sent.
    Blank,
    /// `/exit`.
    Exit,
    /// `/manifest`.
    Manifest,
    /// `/cancel`.
    Cancel,
    /// Anything else: a message to send.
    Prompt(String),
}

fn read_line() -> std::io::Result<Line> {
    let mut raw = String::new();
    let read = std::io::stdin().lock().read_line(&mut raw)?;
    if read == 0 {
        return Ok(Line::Eof);
    }
    let trimmed = raw.trim();
    Ok(match trimmed {
        "" => Line::Blank,
        "/exit" => Line::Exit,
        "/manifest" => Line::Manifest,
        "/cancel" => Line::Cancel,
        prompt => Line::Prompt(prompt.to_owned()),
    })
}

fn print_prompt() {
    print!("> ");
    let _ = std::io::stdout().flush();
}

/// Runs the read-send-report loop until `/exit` or end-of-input.
///
/// # Errors
/// [`harwness_sdk::SdkError`] if opening the session itself fails (a turn
/// that fails on its own is reported, not returned as an error here).
async fn repl_loop(
    ctx: &RunnerContext,
    harwness: harwness_sdk::Harwness,
) -> Result<(), harwness_sdk::SdkError> {
    let mut session = harwness.session()?;
    install_sigint_handler(&session);
    let interactive = std::io::stdin().is_terminal();
    loop {
        if interactive {
            print_prompt();
        }
        let line = match read_line() {
            Ok(line) => line,
            Err(error) => {
                eprintln!("harw-agent-runner: could not read stdin: {error}");
                return Ok(());
            }
        };
        match line {
            Line::Eof | Line::Exit => return Ok(()),
            Line::Blank => continue,
            Line::Manifest => {
                let permissions = &ctx.root_ir().permissions;
                match serde_json::to_string_pretty(permissions) {
                    Ok(text) => println!("{text}"),
                    Err(error) => {
                        eprintln!("harw-agent-runner: could not render manifest: {error}")
                    }
                }
            }
            Line::Cancel => {
                if !session.cancel() {
                    eprintln!("harw-agent-runner: nothing is running to cancel");
                }
            }
            Line::Prompt(prompt) => {
                run_turn(&mut session, prompt).await;
            }
        }
    }
}

/// Wires Ctrl+C to [`harwness_sdk::CancelHandle::cancel`] for the life of
/// the process, so it reaches whichever turn is running when it is pressed
/// (or is a no-op between turns).
fn install_sigint_handler(session: &Session) {
    let cancel = session.cancel_handle();
    // `ctrlc`/`signal-hook` are not workspace dependencies either (see the
    // module doc's `rustyline` note); this crate's `tokio` dependency gained
    // the `signal` feature in this wave specifically for `ctrl_c` below,
    // rather than pulling in a whole extra crate for one handler.
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
            cancel.cancel();
        }
    });
}

/// Runs one turn to completion, streaming it the same way `iface::cli`
/// does, and reports its outcome on stderr.
async fn run_turn(session: &mut Session, prompt: String) {
    let mut events = session.events();
    let printer = tokio::spawn(async move {
        let mut stdout = std::io::stdout();
        while let Some(event) = events.next().await {
            match &event {
                SdkEvent::TextDelta { source, text } if source.is_root() => {
                    let _ = stdout.write_all(text.as_bytes());
                    let _ = stdout.flush();
                }
                SdkEvent::ToolCall {
                    tool, arguments, ..
                } => {
                    dim(&format!("→ {tool}({arguments})"));
                }
                SdkEvent::ToolResult {
                    output, duration, ..
                } => {
                    let outcome = match output {
                        ToolOutput::Success { .. } => "ok".to_owned(),
                        ToolOutput::Error { message } => format!("error: {message}"),
                        // `ToolOutput` is `#[non_exhaustive]`.
                        _ => "unknown".to_owned(),
                    };
                    dim(&format!("← {outcome} ({} ms)", duration.as_millis()));
                }
                _ => {}
            }
            if event.is_root_finish() {
                break;
            }
        }
        let _ = stdout.write_all(b"\n");
    });
    match session.send(prompt).await {
        Ok(report) => {
            let _ = printer.await;
            dim(&format!("[{}]", status_label(&report.status)));
        }
        Err(error) => {
            let _ = printer.await;
            eprintln!("harw-agent-runner: turn error: {error}");
        }
    }
}

fn status_label(status: &TurnStatus) -> String {
    match status {
        TurnStatus::Completed => "completed".to_owned(),
        TurnStatus::Cancelled { reason } => format!("cancelled: {reason}"),
        TurnStatus::Truncated => "truncated".to_owned(),
        TurnStatus::Refused { detail } => match detail {
            Some(detail) => format!("refused: {detail}"),
            None => "refused".to_owned(),
        },
        TurnStatus::Failed { reason } => format!("failed: {reason}"),
        // `TurnStatus` is `#[non_exhaustive]`.
        _ => "unknown".to_owned(),
    }
}

fn dim(line: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("\x1b[2m{line}\x1b[0m");
    } else {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_label_names_every_status() {
        assert_eq!(status_label(&TurnStatus::Completed), "completed");
        assert_eq!(
            status_label(&TurnStatus::Cancelled {
                reason: "user".to_owned()
            }),
            "cancelled: user"
        );
        assert_eq!(status_label(&TurnStatus::Truncated), "truncated");
        assert_eq!(
            status_label(&TurnStatus::Refused { detail: None }),
            "refused"
        );
        assert_eq!(
            status_label(&TurnStatus::Failed {
                reason: "boom".to_owned()
            }),
            "failed: boom"
        );
    }
}
