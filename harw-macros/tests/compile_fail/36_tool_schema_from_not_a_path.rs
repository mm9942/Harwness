// Diagnostic: `schema_from` erwartet einen Pfad, kein String-Literal.
// Expected: `syn`s Parse-Fehler beim Lesen des Pfads.
#![allow(unused_imports)]
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize)]
struct Args {}

#[harw_macros::tool(name = "t.schema", schema_from = "spec_fn")]
async fn t(
    context: &ToolExecutionContext,
    args: Args,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("x"))
}

fn main() {}
