// Diagnostic: zwei `activity(...)`-Schlüssel an einem `#[tool]`.
// Expected: Fehler "duplicate `activity` key".
#![allow(unused_imports)]
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize, harw_macros::Tool)]
struct Args {
    path: String,
}

#[harw_macros::tool(
    name = "t.read",
    activity(domain = "filesystem", verb = "read", resource = "path", effect = "observe"),
    activity(domain = "filesystem", verb = "edit", resource = "path", effect = "mutate")
)]
async fn t(context: &ToolExecutionContext, args: Args) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args.path);
    Ok(ToolOutput::text("x"))
}

fn main() {}
