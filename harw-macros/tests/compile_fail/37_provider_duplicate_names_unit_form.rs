// Diagnostic: zwei `#[tool]`-Typen mit demselben NAME in einem `tool_provider_core!`.
// Expected: Compile-Zeit-Assertion "zwei Tools deklarieren denselben NAME".
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize, harw_macros::Tool)]
struct Args {}

#[harw_macros::tool(name = "dup.name")]
async fn first(
    context: &ToolExecutionContext,
    args: Args,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("1"))
}

#[harw_macros::tool(name = "dup.name")]
async fn second(
    context: &ToolExecutionContext,
    args: Args,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("2"))
}

harw_tools::tool_provider_core! {
    struct DupProvider { FirstTool, SecondTool }
}

fn main() {}
