//! Minimaler Chat: eine Frage, eine Antwort.
//!
//! ```text
//! cargo run -p harwness-sdk --example minimal_chat -- "Was liegt in diesem Repo?"
//! ```
//!
//! Nutzt den konfigurierten Provider aus `~/.harw` (bzw. `HARW_HOME`). Mit
//! gesetztem `HARWNESS_SDK_ECHO` läuft das Beispiel offline gegen den
//! eingebauten Echo-Provider.

use harwness_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.trim().is_empty() {
        "Sag in einem Satz, was du kannst.".to_owned()
    } else {
        prompt
    };

    let mut builder = Harwness::builder();
    if let Ok(reply) = std::env::var("HARWNESS_SDK_ECHO") {
        builder = builder.offline_echo(reply).ephemeral(true);
    }
    let harwness = builder.build()?;

    let mut session = harwness.session()?;
    let report = session.send(prompt).await?;

    match report.status {
        TurnStatus::Completed => println!("{}", report.text.unwrap_or_default()),
        other => eprintln!("turn ended with {other:?}"),
    }
    eprintln!(
        "[session {} | {} tokens | {} tool calls]",
        report.session_id,
        report.usage.total(),
        report.tool_calls
    );
    Ok(())
}
