// Diagnostic: `parallel_safe: [..]` nennt ein Tool, das der Provider nicht deklariert.
// Expected: Compile-Zeit-Assertion "`parallel_safe` nennt ein Tool".
use harw_tools::{ToolOutput, ToolSpec};

struct Existing;

struct Noop;

impl harw_tools::ToolExecutor for Noop {
    fn execute<'a>(
        &'a self,
        _context: &'a harw_tools::ToolExecutionContext,
        _call: &'a harw_tools::ToolCall,
    ) -> harw_tools::ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(ToolOutput::text("x")) })
    }
}

fn spec() -> ToolSpec {
    ToolSpec::Function(harw_tools::FunctionToolSpec {
        name: harw_tools::ToolName::new("a.tool"),
        description: String::new(),
        parameters: harw_tools::JsonSchema::default(),
        strict: false,
    })
}

harw_tools::tool_provider_core! {
    impl for Existing as provider, parallel_safe: ["a.typo"] {
        "a.tool" => { spec: spec(), executor: Noop }
    }
}

fn main() {}
