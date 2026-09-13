// Diagnose 8: `#[operation(agent_tool(authority = "..."))]` mit einer Kennung,
// die keine Laufzeit auflösen kann.
//
// Warum das ein Compile-Fehler sein muss, obwohl die Laufzeit fail-closed ist:
// `harw_core_bridge::resolve_authority_reducer` bildet unbekannte Kennungen auf
// `reduce_to_read_only` ab. Das verhindert eine Rechteausweitung, aber nicht die
// Gegenrichtung — ein Tippfehler in `reduce_to_read_execute` degradiert ein
// Kind, das ausführen darf, still auf read-only. Die Operation ist dann
// funktionsunfähig, und nirgends erscheint ein Fehler.
//
// Der Fixture ist bewusst minimal: keine `use`-Zeilen, kein `OpArgs`-Derive.
// Ein Compile-Fail-Fixture soll **genau eine** Diagnose auslösen — trybuild
// vergleicht die vollständige stderr-Ausgabe, und jeder Nebenfehler machte den
// Snapshot von unabhängigen Änderungen abhängig.
//
// Erwartet: compile_error! "unbekannter authority-Reducer ...; erlaubt sind: ..."

#[derive(Default, serde::Deserialize)]
struct ExploreArgs {}

#[harw_macros::operation(
    name = "explore_typo",
    summary = "Kind mit falsch geschriebenem Autoritäts-Reducer.",
    domain = "agents",
    permission = "operator",
    command(path = "/explore-typo", visibility = "tui_only"),
    agent_tool(
        child = "explorer",
        authority = "reduce_to_read_excute",
        budget = "8k_tokens,20_tool_calls,30s"
    )
)]
async fn explore_typo(
    _ctx: &harw_operations::OpContext,
    _args: ExploreArgs,
) -> Result<harw_operations::OpOutput, harw_operations::OpError> {
    Ok(harw_operations::OpOutput {
        text: String::new(),
    })
}

fn main() {}
