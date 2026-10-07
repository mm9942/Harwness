# harwness-sdk

A stable, documented API for embedding Harwness in your own programs.

The SDK is the **semver boundary** in front of the internal `harw-*` crates:
no public signature names an internal type. JSON payloads travel as
`serde_json::Value` (re-exported as `harwness_sdk::serde_json`).

## Quick start

```rust,no_run
use harwness_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    let harwness = Harwness::builder().cwd(".").build()?;
    let mut session = harwness.session()?;
    let report = session.send("Which tests are there?").await?;
    println!("{}", report.text.unwrap_or_default());
    Ok(())
}
```

Prerequisites: a Tokio runtime and a set-up root space
(`~/.harw` or `HARW_HOME`) with an active UIA and a configured provider —
`harw` sets up both on first start. For tests without network:
`HarwnessBuilder::offline_echo("…")`.

## API overview

| Component | Purpose |
|---|---|
| `Harwness::builder()` → `HarwnessBuilder` | `home`, `cwd`, `model`, `provider`, `mode`, `reasoning_effort`, `agent`, `approval_policy`, `approval_handler`, `tool`, `context_source`, `ephemeral`, `scaffold_home`, `offline_echo` |
| `HarwnessBuilder::build()` | validates inputs, loads configuration (with repo trust check), checks UIA/provider, opens the history store |
| `Harwness::session()` / `Harwness::resume(&id)` | new or saved session |
| `Session::send(text)` → `TurnReport` | runs a turn to completion, including approvals and child agents |
| `Session::events()` → `EventStream` | `SdkEvent`s: `TurnStarted`, `TextDelta`, `ReasoningDelta`, `Message`, `ToolCall`, `ToolResult`, `ChildSpawned`, `ChildCompleted`, `Usage`, `Context`, `Error`, `Finished`, `Lagged` |
| `Session::cancel_handle()` → `CancelHandle` | cancel a running turn from outside |
| `Session::history()`, `Session::total_usage()` | history and usage |
| `Tool`, `FnTool`, `ToolContext`, `ToolError` | custom tools (JSON in, JSON out) |
| `ContextSource`, `ContextItem` | custom context per turn (trust class "data") |
| `ApprovalHandler`, `approval_fn`, `AutoDeny`, `Decision`, `ApprovalPolicy` | approvals; default `AutoDeny` + `Delegated` |
| `SdkError` | a single error type, `#[non_exhaustive]` |

## Approvals

The runtime pauses tool calls that its policy does not let through without
asking (`ApprovalPolicy::Delegated`: anything that modifies or executes;
`AlwaysAsk`: everything; `FullAccess`: nothing). `Session::send` presents
each such request to the `ApprovalHandler`. Without a handler of your own,
`AutoDeny` denies; a request is also denied if no answer arrives within the
configured approval timeout or if the turn is cancelled.

## Examples

```text
cargo run -p harwness-sdk --example minimal_chat -- "What is in here?"
cargo run -p harwness-sdk --example streaming_events -- "Analyze src/"
cargo run -p harwness-sdk --example custom_tool
```

## Features

- `browser` (default): browser tools of the root session, if configured.
- `unstable-internals`: raw extension points (`raw_tool_provider`,
  `raw_context_provider`, `raw_model_provider`, `raw_secret_resolver`) —
  **without** a semver guarantee.

## Limits (as of this version)

- The entry point uses the interactive runtime profile (`EntryKind::Tui`);
  an active UIA is therefore mandatory.
- Configuration is loaded from the files of the root space and the project;
  in-memory overrides exist only for model, provider, mode, effort, agent
  and approval policy.
- `auth = "secrets:…"` needs a resolver (only via
  `unstable-internals`).
- The SDK does not drive nested pauses of child agents (their own
  approvals/handoffs); the child then ends with an error result.
