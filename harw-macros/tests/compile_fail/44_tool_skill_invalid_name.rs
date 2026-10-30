// Diagnostic: `skill = "..."` verletzt die Skill-Namensregel von harw-catalog.
// Expected: Fehler "is not a valid skill name" am Literal.
#![allow(unused_imports)]
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize, harw_macros::Tool)]
struct Args {}

#[harw_macros::tool(name = "t.read", skill = "Fsread_Tools")]
async fn t(context: &ToolExecutionContext, args: Args) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("x"))
}

fn main() {}
