// Diagnostic: `#[tool(state = ..)]` mit einem Zustand als Wert statt als `&State`.
// Expected: Fehler "must be a reference to the state".
#![allow(unused_imports)]
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize, harw_macros::Tool)]
struct Args {}

#[harw_macros::tool(name = "t.state", state = u32)]
async fn t(
    state: u32,
    context: &ToolExecutionContext,
    args: Args,
) -> Result<ToolOutput, ToolsError> {
    let _ = (state, context, args);
    Ok(ToolOutput::text("x"))
}

fn main() {}
