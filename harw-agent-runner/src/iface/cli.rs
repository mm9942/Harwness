//! `cli`: the one-shot, non-interactive interface (feature `cli`).
//!
//! # Description
//! Runs exactly one turn: the prompt comes from [`crate::args::RunnerArgs::prompt`]
//! (the positional words on the command line) or, if that is absent, from
//! stdin read to end-of-file. Assistant text streams to stdout as it
//! arrives; tool calls and their results are dim lines on stderr, so stdout
//! stays exactly the agent's answer for a shell pipeline (`harw-agent-run |
//! less`, `... > answer.txt`). `--json` replaces both with one JSON object
//! per [`harwness_sdk::SdkEvent`] on stdout instead.
//!
//! Approval uses [`super::approval::TerminalApproval`]: interactive by
//! default (asks at the terminal), or, with `--full-access`, approves
//! everything the manifest already allows without asking.
//!
//! # Exit codes
//! - `0`: the turn completed.
//! - `1`: the turn failed, was refused, or was truncated (anything else
//!   `harwness_sdk::TurnStatus` reports besides the three below), or no
//!   prompt was given at all.
//! - `2`: the turn was cancelled (nothing cancels it here — this interface
//!   runs to completion — but a budget or shutdown cancellation from the
//!   runtime itself still reports this way).
//! - `3`: at least one tool call was denied approval (by the user, or for
//!   lack of a terminal) — reported even if the turn otherwise completed,
//!   since the answer is then incomplete by construction.

use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;
use std::sync::Arc;

use harwness_sdk::{EventSource, FinishStatus, SdkEvent, ToolOutput, TurnStatus, Usage};

use super::approval::{TerminalApproval, build_harwness};
use crate::context::RunnerContext;

const EXIT_COMPLETED: u8 = 0;
const EXIT_FAILED: u8 = 1;
const EXIT_CANCELLED: u8 = 2;
const EXIT_APPROVAL_DENIED: u8 = 3;

/// Entry point for `iface::Interface` (feature `cli`).
pub fn run(ctx: RunnerContext) -> ExitCode {
    let prompt = match resolve_prompt(&ctx) {
        Ok(prompt) => prompt,
        Err(message) => {
            eprintln!("harw-agent-runner: {message}");
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let json = ctx.args.json;
    let approvals = Arc::new(TerminalApproval::new(ctx.args.flags.full_access));
    let harwness = match build_harwness(&ctx, approvals.clone()) {
        Ok(harwness) => harwness,
        Err(error) => {
            eprintln!("harw-agent-runner: {error}");
            return ExitCode::from(EXIT_FAILED);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("harw-agent-runner: could not start the async runtime: {error}");
            return ExitCode::from(EXIT_FAILED);
        }
    };

    let status = runtime.block_on(run_one_shot(harwness, prompt, json));
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            eprintln!("harw-agent-runner: {error}");
            return ExitCode::from(EXIT_FAILED);
        }
    };

    if approvals.any_denied() {
        return ExitCode::from(EXIT_APPROVAL_DENIED);
    }
    ExitCode::from(match status {
        TurnStatus::Completed => EXIT_COMPLETED,
        TurnStatus::Cancelled { .. } => EXIT_CANCELLED,
        // `TurnStatus` is `#[non_exhaustive]`: `Truncated`/`Refused`/`Failed`
        // and anything a later SDK version adds all count as "failed" here.
        _ => EXIT_FAILED,
    })
}

/// The prompt from `--` / positional words, or all of stdin if neither is
/// given.
///
/// # Errors
/// A message naming the problem if stdin cannot be read, or if there was
/// no prompt at all (empty after trimming).
fn resolve_prompt(ctx: &RunnerContext) -> Result<String, String> {
    if let Some(prompt) = &ctx.args.prompt {
        return Ok(prompt.clone());
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|error| format!("could not read the prompt from stdin: {error}"))?;
    if text.trim().is_empty() {
        return Err(
            "no prompt: give one on the command line or pipe it on stdin".to_owned(),
        );
    }
    Ok(text)
}

/// Drives one turn, streaming its events, and returns how it ended.
async fn run_one_shot(
    harwness: harwness_sdk::Harwness,
    prompt: String,
    json: bool,
) -> Result<TurnStatus, harwness_sdk::SdkError> {
    let mut session = harwness.session()?;
    let mut events = session.events();
    let printer = tokio::spawn(async move {
        let mut streamed_root_text = false;
        let mut stdout = std::io::stdout();
        while let Some(event) = events.next().await {
            if json {
                if let Ok(line) = serde_json::to_string(&event_json(&event)) {
                    println!("{line}");
                }
                if event.is_root_finish() {
                    break;
                }
                continue;
            }
            match &event {
                SdkEvent::TextDelta { source, text } if source.is_root() => {
                    streamed_root_text = true;
                    let _ = stdout.write_all(text.as_bytes());
                    let _ = stdout.flush();
                }
                SdkEvent::Message {
                    source,
                    text,
                    final_answer,
                } if source.is_root() && *final_answer && !streamed_root_text => {
                    // No provider streaming happened for the root: this is
                    // the only chance to print the answer at all.
                    let _ = stdout.write_all(text.as_bytes());
                    let _ = stdout.flush();
                }
                SdkEvent::ToolCall {
                    source,
                    tool,
                    arguments,
                    ..
                } => {
                    eprintln_dim(&format!(
                        "{}→ {tool}({arguments})",
                        role_prefix(source)
                    ));
                }
                SdkEvent::ToolResult {
                    source,
                    output,
                    duration,
                    ..
                } => {
                    let outcome = match output {
                        ToolOutput::Success { .. } => "ok".to_owned(),
                        ToolOutput::Error { message } => format!("error: {message}"),
                        // `ToolOutput` is `#[non_exhaustive]`.
                        _ => "unknown".to_owned(),
                    };
                    eprintln_dim(&format!(
                        "{}← {outcome} ({} ms)",
                        role_prefix(source),
                        duration.as_millis()
                    ));
                }
                SdkEvent::Error { message, .. } => {
                    eprintln_dim(&format!("! {message}"));
                }
                _ => {}
            }
            if event.is_root_finish() {
                break;
            }
        }
        if !json && streamed_root_text {
            let _ = stdout.write_all(b"\n");
        }
    });

    let report = session.send(prompt).await?;
    let _ = printer.await;
    Ok(report.status)
}

fn role_prefix(source: &EventSource) -> String {
    role_prefix_parts(source.is_root(), &source.role)
}

/// [`role_prefix`]'s actual logic, taking `EventSource`'s two relevant
/// fields directly. `EventSource` is a `#[non_exhaustive]` struct with no
/// public constructor, so this crate cannot build one to test against —
/// this split keeps the logic itself testable from here regardless.
fn role_prefix_parts(is_root: bool, role: &str) -> String {
    if is_root {
        String::new()
    } else {
        format!("[{role}] ")
    }
}

/// Dims a line on stderr when stderr is a terminal; plain text otherwise
/// (piped/redirected stderr, e.g. into a log file).
fn eprintln_dim(line: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("\x1b[2m{line}\x1b[0m");
    } else {
        eprintln!("{line}");
    }
}

/// [`ToolOutput`]'s JSON shape, factored out of [`event_json`] so it is
/// testable on its own: `ToolOutput` (unlike [`EventSource`]) is an enum
/// whose existing variants a downstream crate can still construct even
/// though the enum itself is `#[non_exhaustive]`.
fn tool_output_json(output: &ToolOutput) -> serde_json::Value {
    match output {
        ToolOutput::Success { value } => serde_json::json!({"success": true, "value": value}),
        ToolOutput::Error { message } => serde_json::json!({"success": false, "message": message}),
        // `ToolOutput` is `#[non_exhaustive]`.
        _ => serde_json::json!({"success": false, "message": "unknown tool output"}),
    }
}

/// [`FinishStatus`]'s JSON label, factored out for the same reason as
/// [`tool_output_json`].
fn finish_status_str(status: &FinishStatus) -> &'static str {
    match status {
        FinishStatus::Completed => "completed",
        FinishStatus::Aborted => "aborted",
        // `FinishStatus` is `#[non_exhaustive]`.
        _ => "unknown",
    }
}

/// Renders one [`SdkEvent`] as JSON. `harwness_sdk`'s event types carry no
/// `Serialize` impl (they are not meant to be a wire format), so `--json`
/// builds its own `serde_json::Value` by hand instead of deriving one.
fn event_json(event: &SdkEvent) -> serde_json::Value {
    fn source_json(source: &EventSource) -> serde_json::Value {
        serde_json::json!({
            "session_id": source.session_id.as_str(),
            "parent": source.parent.as_ref().map(harwness_sdk::SessionId::as_str),
            "role": source.role,
        })
    }
    fn usage_json(usage: &Usage) -> serde_json::Value {
        serde_json::json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "reasoning_tokens": usage.reasoning_tokens,
            "cached_tokens": usage.cached_tokens,
            "cache_write_tokens": usage.cache_write_tokens,
        })
    }
    match event {
        SdkEvent::TurnStarted { source, turn_id } => serde_json::json!({
            "type": "turn_started", "source": source_json(source), "turn_id": turn_id,
        }),
        SdkEvent::TextDelta { source, text } => serde_json::json!({
            "type": "text_delta", "source": source_json(source), "text": text,
        }),
        SdkEvent::ReasoningDelta { source, text } => serde_json::json!({
            "type": "reasoning_delta", "source": source_json(source), "text": text,
        }),
        SdkEvent::Message {
            source,
            text,
            final_answer,
        } => serde_json::json!({
            "type": "message", "source": source_json(source), "text": text,
            "final_answer": final_answer,
        }),
        SdkEvent::ToolCall {
            source,
            call_id,
            tool,
            arguments,
        } => serde_json::json!({
            "type": "tool_call", "source": source_json(source), "call_id": call_id,
            "tool": tool, "arguments": arguments,
        }),
        SdkEvent::ToolResult {
            source,
            call_id,
            output,
            duration,
        } => serde_json::json!({
            "type": "tool_result",
            "source": source_json(source),
            "call_id": call_id,
            "duration_ms": duration.as_millis() as u64,
            "output": tool_output_json(output),
        }),
        SdkEvent::ChildSpawned {
            source,
            child,
            role,
            task,
        } => serde_json::json!({
            "type": "child_spawned", "source": source_json(source), "child": child.as_str(),
            "role": role, "task": task,
        }),
        SdkEvent::ChildCompleted {
            source,
            child,
            outcome,
            duration,
        } => serde_json::json!({
            "type": "child_completed", "source": source_json(source), "child": child.as_str(),
            "outcome": outcome, "duration_ms": duration.as_millis() as u64,
        }),
        SdkEvent::Usage {
            source,
            round,
            turn_total,
            final_round,
        } => serde_json::json!({
            "type": "usage", "source": source_json(source), "round": usage_json(round),
            "turn_total": usage_json(turn_total), "final_round": final_round,
        }),
        SdkEvent::Context {
            source,
            used_tokens,
            window_tokens,
        } => serde_json::json!({
            "type": "context", "source": source_json(source), "used_tokens": used_tokens,
            "window_tokens": window_tokens,
        }),
        SdkEvent::Error {
            source,
            message,
            retryable,
        } => serde_json::json!({
            "type": "error", "source": source_json(source), "message": message,
            "retryable": retryable,
        }),
        SdkEvent::Finished {
            source,
            status,
            usage,
        } => serde_json::json!({
            "type": "finished",
            "source": source_json(source),
            "status": finish_status_str(status),
            "usage": usage.as_ref().map(usage_json),
        }),
        SdkEvent::Lagged { skipped } => serde_json::json!({
            "type": "lagged", "skipped": skipped,
        }),
        // `SdkEvent` is `#[non_exhaustive]`: a later SDK version's new
        // variant still becomes a valid (if uninformative) JSON line.
        _ => serde_json::json!({"type": "unknown"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `EventSource` and `SdkEvent` are `#[non_exhaustive]` with no public
    // constructor `harwness-sdk` exposes (a real one only ever comes from a
    // live session), so `event_json`'s outer dispatch and `role_prefix`'s
    // outer wrapper cannot be exercised with fabricated values from this
    // crate. What they *do* besides destructure-and-delegate — the JSON
    // shape of a tool result, a finish status, and the role-prefix rule
    // itself — is fully testable through the plain-data helpers they
    // delegate to, below.

    #[test]
    fn test_tool_output_json_lines_are_valid_json_and_tagged() {
        let outputs = [
            ToolOutput::Success {
                value: serde_json::json!({"lines": 3}),
            },
            ToolOutput::Error {
                message: "not found".to_owned(),
            },
        ];
        for output in &outputs {
            let value = tool_output_json(output);
            let line = serde_json::to_string(&value).expect("serializable");
            let reparsed: serde_json::Value =
                serde_json::from_str(&line).expect("valid json line");
            assert!(reparsed["success"].is_boolean(), "{line}");
        }
        assert_eq!(
            tool_output_json(&ToolOutput::Error {
                message: "nope".to_owned()
            })["message"],
            "nope"
        );
    }

    #[test]
    fn test_finish_status_str_names_every_status() {
        assert_eq!(finish_status_str(&FinishStatus::Completed), "completed");
        assert_eq!(finish_status_str(&FinishStatus::Aborted), "aborted");
    }

    #[test]
    fn test_role_prefix_parts_is_empty_for_the_root() {
        assert_eq!(role_prefix_parts(true, "assistant"), "");
        assert_eq!(role_prefix_parts(false, "explorer"), "[explorer] ");
    }
}
