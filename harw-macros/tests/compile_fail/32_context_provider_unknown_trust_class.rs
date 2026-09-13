// Diagnostic: `#[context_provider(trust = Bogus)]` — unbekannte TrustClass.
// Expected: compile_error! "unknown trust class `Bogus`; expected one of: ..."
use harw_macros::context_provider;

#[context_provider(trust = Bogus)]
async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
    Vec::new()
}

fn main() {}
