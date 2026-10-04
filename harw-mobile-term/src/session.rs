//! The loop: typed lines in, frames in, text out.
//!
//! Input arrives on a channel (the binary feeds it from stdin), output goes to
//! any [`Write`], so the whole loop runs in tests without a terminal.
//!
//! Waiting for a frame is cancelled when a line arrives first. That is safe:
//! the frame queue of an attachment hands a frame out under a lock and only
//! then returns, and [`Controller::next`] retries an interrupted re-attach.

use std::io::{self, Write};

use harw_mobile_core::Controller;
use harw_protocol::PortError;
use harw_protocol::session_wire::{RespondResult, SubmitResult};
use harw_types::ReviewDecision;
use tokio::sync::mpsc;

use crate::command::{Command, HELP, parse};
use crate::render::{DEFAULT_WIDTH, Printer};

/// Why [`run`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// The user left (`/q`).
    Quit,
    /// The input channel closed (stdin ended).
    InputClosed,
    /// The attachment ended or failed; the message says how.
    Lost(String),
}

fn say(out: &mut dyn Write, lines: &[String]) -> io::Result<()> {
    for line in lines {
        writeln!(out, "{line}")?;
    }
    out.flush()
}

/// The open approval a command refers to: the given 1-based number, or the
/// only one.
fn target(controller: &Controller, index: Option<usize>) -> Result<harw_types::ApprovalId, String> {
    let open: Vec<_> = controller.view().pending_approvals().collect();
    match (index, open.len()) {
        (_, 0) => Err("no open approval".to_owned()),
        (None, 1) => Ok(open[0].id.clone()),
        (None, n) => Err(format!(
            "{n} open approvals; say which, like /y 2 (/p lists them)"
        )),
        (Some(i), n) if i <= n => Ok(open[i - 1].id.clone()),
        (Some(i), n) => Err(format!("no approval {i}; there are {n}")),
    }
}

fn pending_lines(controller: &Controller) -> Vec<String> {
    let open: Vec<_> = controller.view().pending_approvals().collect();
    if open.is_empty() {
        return vec!["no open approvals".to_owned()];
    }
    open.iter()
        .enumerate()
        .map(|(n, a)| format!("[{}] {}", n + 1, a.summary))
        .collect()
}

fn port_error(error: &PortError) -> String {
    format!("! {error}")
}

/// Runs one command; returns `true` when the user wants to leave.
async fn handle(
    controller: &mut Controller,
    command: Command,
    out: &mut dyn Write,
) -> io::Result<bool> {
    match command {
        Command::Quit => return Ok(true),
        Command::Help => say(out, &HELP.lines().map(str::to_owned).collect::<Vec<_>>())?,
        Command::Invalid(message) => say(out, &[format!("? {message}")])?,
        Command::Pending => say(out, &pending_lines(controller))?,
        Command::Interrupt => {
            let line = match controller.interrupt().await {
                Ok(()) => "- interrupt sent".to_owned(),
                Err(error) => port_error(&error),
            };
            say(out, &[line])?;
        }
        Command::Send(text) => {
            let line = match controller.submit(text).await {
                Ok(SubmitResult::Accepted { position: 0 }) => None,
                Ok(SubmitResult::Accepted { position }) => {
                    Some(format!("- queued ({position} ahead)"))
                }
                Ok(SubmitResult::Stale { .. }) => {
                    Some("! the session moved on; read it and send again".to_owned())
                }
                Ok(SubmitResult::QueueFull) => Some("! the queue is full".to_owned()),
                Ok(SubmitResult::Denied { reason }) => Some(format!("! not allowed: {reason}")),
                Err(error) => Some(port_error(&error)),
            };
            if let Some(line) = line {
                say(out, &[line])?;
            }
        }
        Command::Approve(index) => {
            decide(controller, index, ReviewDecision::Approved, None, out).await?
        }
        Command::Deny(index, reason) => {
            decide(controller, index, ReviewDecision::Rejected, reason, out).await?;
        }
    }
    Ok(false)
}

async fn decide(
    controller: &Controller,
    index: Option<usize>,
    decision: ReviewDecision,
    reason: Option<String>,
    out: &mut dyn Write,
) -> io::Result<()> {
    let id = match target(controller, index) {
        Ok(id) => id,
        Err(message) => return say(out, &[format!("? {message}")]),
    };
    let line = match controller.decide(id, decision, reason).await {
        Ok(RespondResult::Resolved) => return Ok(()),
        Ok(RespondResult::AlreadyResolved { by }) => format!("- already decided by {by}"),
        Ok(RespondResult::Expired) => "- that approval expired".to_owned(),
        Ok(RespondResult::Denied { reason }) => format!("! not allowed: {reason}"),
        Err(error) => port_error(&error),
    };
    say(out, &[line])
}

/// Runs the session loop until the user leaves, the input ends, or the
/// attachment is lost.
///
/// # Errors
/// Only an I/O error on `out`.
pub async fn run(
    controller: &mut Controller,
    mut input: mpsc::Receiver<String>,
    out: &mut dyn Write,
) -> io::Result<RunEnd> {
    let mut printer = Printer::new(DEFAULT_WIDTH);
    say(out, &printer.update(controller.view()))?;
    loop {
        tokio::select! {
            line = input.recv() => {
                let Some(line) = line else { return Ok(RunEnd::InputClosed) };
                if let Some(command) = parse(&line) {
                    if handle(controller, command, out).await? {
                        return Ok(RunEnd::Quit);
                    }
                }
                say(out, &printer.update(controller.view()))?;
            }
            frame = controller.next() => match frame {
                Ok(true) => say(out, &printer.update(controller.view()))?,
                Ok(false) => {
                    say(out, &["! the host closed the session stream".to_owned()])?;
                    return Ok(RunEnd::Lost("stream ended".to_owned()));
                }
                Err(error) => {
                    say(out, &[port_error(&error)])?;
                    return Ok(RunEnd::Lost(error.to_string()));
                }
            },
        }
    }
}
