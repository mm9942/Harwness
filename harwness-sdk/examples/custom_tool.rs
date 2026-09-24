//! Eigene Werkzeuge, eigener Kontext und eigene Freigaben.
//!
//! ```text
//! cargo run -p harwness-sdk --example custom_tool
//! ```
//!
//! - `host.word_count` als [`FnTool`] (Closure),
//! - `host.inventory` als eigener [`Tool`]-Typ mit Zustand,
//! - eine [`ContextSource`], die jedem Turn Host-Wissen mitgibt,
//! - ein [`approval_fn`]-Handler: Host-Werkzeuge ja, alles andere nein.

use std::collections::BTreeMap;

use harwness_sdk::prelude::*;
use harwness_sdk::serde_json::{Value, json};

/// Ein Werkzeug mit Zustand: ein kleines Lager.
struct Inventory {
    stock: BTreeMap<&'static str, u32>,
}

impl Tool for Inventory {
    fn name(&self) -> &str {
        "host.inventory"
    }

    fn description(&self) -> &str {
        "Liefert den Lagerbestand eines Artikels (sku) oder aller Artikel."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sku": { "type": "string", "description": "Artikelnummer; leer = alle" }
            }
        })
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ToolContext,
        arguments: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            if ctx.is_cancelled() {
                return Err(ToolError::new("cancelled"));
            }
            match arguments.get("sku").and_then(Value::as_str) {
                Some(sku) => self
                    .stock
                    .get(sku)
                    .map(|count| json!({ "sku": sku, "count": count }))
                    .ok_or_else(|| ToolError::new(format!("unknown sku '{sku}'"))),
                None => harwness_sdk::serde_json::to_value(&self.stock)
                    .map_err(|error| ToolError::new(error.to_string())),
            }
        })
    }
}

/// Gibt jedem Turn den Namen des Hosts mit.
struct HostFacts;

impl ContextSource for HostFacts {
    fn namespace(&self) -> &'static str {
        "host.facts"
    }

    fn items<'a>(&'a self, _session_id: &'a SessionId) -> BoxFuture<'a, Vec<ContextItem>> {
        Box::pin(async {
            vec![ContextItem::new(
                "host.name",
                "Dieses Programm ist das Lagerverwaltungs-Demo der harwness-sdk.",
            )]
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let word_count = FnTool::new(
        "host.word_count",
        "Zählt die Wörter eines Texts.",
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        }),
        |arguments: Value| async move {
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::new("'text' is required"))?;
            Ok::<_, ToolError>(json!(text.split_whitespace().count()))
        },
    )
    .with_parallel_safe(true);

    let inventory = Inventory {
        stock: BTreeMap::from([("A-1", 12), ("B-7", 0)]),
    };

    // Host-Werkzeuge freigeben, alles andere (Dateien, Shell, …) ablehnen.
    let approvals = approval_fn(|request: ApprovalRequest| async move {
        if request.tool.starts_with("host.") {
            Decision::Approve
        } else {
            Decision::deny(format!("{} is not allowed in this demo", request.tool))
        }
    });

    let harwness = Harwness::builder()
        .tool(word_count)
        .tool(inventory)
        .context_source(HostFacts)
        .approval_policy(ApprovalPolicy::AlwaysAsk)
        .approval_handler(approvals)
        .build()?;
    eprintln!("tools: {:?}", harwness.tool_names());

    let mut session = harwness.session()?;
    let report = session
        .send("Wie viele Wörter hat 'eins zwei drei', und ist Artikel B-7 vorrätig?")
        .await?;
    println!("{}", report.text.unwrap_or_default());
    eprintln!(
        "[{:?} | {} tool calls | {} approvals]",
        report.status, report.tool_calls, report.approvals
    );
    Ok(())
}
