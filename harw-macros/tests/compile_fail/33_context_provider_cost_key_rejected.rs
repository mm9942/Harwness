// Diagnostic: `#[context_provider(cost = BytesOverFour)]` — `cost` existiert
// absichtlich NICHT als Attributschlüssel (siehe contributor.rs-Moduldoku,
// Abschnitt "Namensraum, Vertrauensklasse und Kosten").
// Expected: compile_error! "unknown `context_provider` attribute field"
use harw_macros::context_provider;

#[context_provider(cost = BytesOverFour)]
async fn goal_context(ctx: &TurnInputContext) -> Vec<ContextFragment> {
    Vec::new()
}

fn main() {}
