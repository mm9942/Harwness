// Diagnostic: `#[context_provider(namespace = "My.Namespace")]` — Zeichen
// außerhalb [a-z0-9_] (Großbuchstabe und Punkt).
// Expected: compile_error! "contains invalid character"
use harw_macros::context_provider;

#[context_provider(namespace = "My.Namespace")]
async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
    Vec::new()
}

fn main() {}
