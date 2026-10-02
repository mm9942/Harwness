// Diagnostic: `#[tool(state = ..)]` auf einer Funktion ohne Zustands-Argument.
// Expected: Fehler, der die Signatur `(state: &State, context, args)` nennt.
#![allow(unused_imports)]
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize, harw_macros::Tool)]
struct Args {}

#[harw_macros::tool(name = "t.state", state = u32)]
async fn t(
    context: &ToolExecutionContext,
    args: Args,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("x"))
}

fn main() {}
