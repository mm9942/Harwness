//! Live-Ereignisse: Textdeltas, Werkzeugaufrufe und Kind-Agenten mitlesen,
//! während der Turn läuft; Ctrl+C bricht den Turn ab.
//!
//! ```text
//! cargo run -p harwness-sdk --example streaming_events -- "Analysiere src/"
//! ```

use std::io::Write as _;
use std::time::Duration;

use harwness_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.trim().is_empty() {
        "Liste die wichtigsten Dateien dieses Projekts auf.".to_owned()
    } else {
        prompt
    };

    let harwness = Harwness::builder().build()?;
    let mut session = harwness.session()?;

    // Abbruch von außen: Ctrl+C beendet den laufenden Turn sauber.
    let cancel = session.cancel_handle();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel.cancel();
        }
    });

    // Der Strom sieht nur Ereignisse ab jetzt: vor `send` abonnieren.
    let mut events = session.events();
    let printer = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            let root = event.source().is_some_and(|source| source.is_root());
            match &event {
                SdkEvent::TextDelta { text, .. } if root => {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
                SdkEvent::ToolCall { tool, source, .. } => {
                    eprintln!("\n[{}] -> {tool}", source.role);
                }
                SdkEvent::ToolResult {
                    output, duration, ..
                } => {
                    let state = if output.is_success() { "ok" } else { "error" };
                    eprintln!("[tool] <- {state} ({} ms)", duration.as_millis());
                }
                SdkEvent::ChildSpawned { role, task, .. } => {
                    eprintln!("[child] {role}: {}", task.as_deref().unwrap_or("-"));
                }
                SdkEvent::ChildCompleted { child, outcome, .. } => {
                    eprintln!("[child] {child} finished: {outcome}");
                }
                SdkEvent::Context {
                    used_tokens,
                    window_tokens,
                    ..
                } => eprintln!("[context] {used_tokens}/{window_tokens}"),
                SdkEvent::Lagged { skipped } => eprintln!("[lagged] {skipped} events lost"),
                _ => {}
            }
            if event.is_root_finish() {
                break;
            }
        }
    });

    let report = session.send(prompt).await?;
    // Endet der Turn ohne Finish-Ereignis (etwa bei einem Kernfehler), soll
    // der Drucker nicht ewig warten.
    let _ = tokio::time::timeout(Duration::from_secs(2), printer).await;

    println!();
    eprintln!("status: {:?}, usage: {:?}", report.status, report.usage);
    Ok(())
}
