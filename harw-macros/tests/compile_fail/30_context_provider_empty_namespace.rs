// Diagnostic: `#[context_provider(namespace = "")]` — leerer Namensraum.
// Expected: compile_error! "context_provider namespace must not be empty"
use harw_macros::context_provider;

#[context_provider(namespace = "")]
async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
    Vec::new()
}

fn main() {}
