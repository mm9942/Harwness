// Diagnostic: `activity(...)` ohne Pflichtschlüssel `verb`.
// Expected: Fehler "`activity(...)` requires `verb = \"...\"`".
#![allow(unused_imports)]
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize, harw_macros::Tool)]
struct Args {
    path: String,
}

#[harw_macros::tool(
    name = "t.read",
    activity(domain = "filesystem", resource = "path", effect = "observe")
)]
async fn t(context: &ToolExecutionContext, args: Args) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args.path);
    Ok(ToolOutput::text("x"))
}

fn main() {}
