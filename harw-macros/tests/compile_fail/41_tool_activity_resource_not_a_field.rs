// Diagnostic: `activity(resource = "...")` nennt kein Feld des Args-Typs.
// Expected: rustc-Fehler "no field `file` on type `&Args`" an der Span des
// `resource`-Literals (generierte Prüffunktion).
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};

#[derive(serde::Deserialize, harw_macros::Tool)]
struct Args {
    path: String,
}

#[harw_macros::tool(
    name = "t.read",
    activity(domain = "filesystem", verb = "read", resource = "file", effect = "observe")
)]
async fn t(context: &ToolExecutionContext, args: Args) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args.path);
    Ok(ToolOutput::text("x"))
}

fn main() {}
