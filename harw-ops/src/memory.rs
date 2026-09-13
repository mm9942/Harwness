//! `/memory` — Long-Term-Memory-Operation.
//!
//! # Verantwortungsbereich
//! Exponiert das `harw-memory`-Backend als `/memory`-Command (channel_parity).
//! Die Operation wird derzeit nicht als Model-Tool exponiert, weil ihr
//! invocation-neutraler Callback auch mutierende Subcommands (`record …`,
//! `maintain`) ausführt.
//!
//! # Subcommands
//! - `list` (Default) — gibt den HOT-Tier komplett zurück (≤100 Zeilen).
//! - `stats` — Zähler-Statistik über HOT/WARM/COLD und offene Signale.
//! - `recall <keywords…>` — Namespace-frei über WARM suchen.
//! - `record correction <text…>` — hängt eine Korrektur an das Signal-Log an.
//! - `record reflection <context> :: <lesson>` — hängt eine Reflexion an.
//! - `maintain` — führt den idempotenten Konsolidierungslauf aus.
//!
//! # Nebenläufigkeit
//! Die Op selbst ist zustandslos; das eigentliche Backend
//! (`Arc<dyn harw_memory::Memory>`) wird aus [`OpContext::service`] aufgelöst.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Memory-Backend im Kontext registriert.
//! - [`OpError::InvalidArguments`] — Argumentgrammatik verletzt.
//! - [`OpError::Execution`] — Backend-Fehler (I/O, Serde, Tier-Overflow, Lock).

use std::sync::Arc;

use harw_macros::operation;
use harw_memory::{Memory, RecallQuery, Signal};
use harw_operations::{OpContext, OpError, OpOutput};

/// Argument-Container für `/memory`.
///
/// # Beschreibung
/// Positional: `sub` ist das Subcommand, `tail` sind die restlichen Tokens.
/// Die Command-Fläche parst über [`FromRawArgs`].
#[derive(Default, serde::Deserialize)]
pub struct MemoryArgs {
    /// Subcommand: `list`, `stats`, `recall`, `record`, `maintain`.
    #[serde(default)]
    pub sub: Option<String>,
    /// Weitere Tokens nach dem Subcommand (Keywords, Text, …).
    #[serde(default)]
    pub tail: Vec<String>,
}

impl harw_operations::FromRawArgs for MemoryArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        Ok(Self {
            sub: tokens.first().cloned(),
            tail: tokens.iter().skip(1).cloned().collect(),
        })
    }
}

/// Führt den `/memory`-Subcommand aus.
///
/// # Argumente
/// - `ctx` — Ausführungskontext; muss `Arc<dyn Memory>` als Service anbieten.
/// - `args` — bereits geparst (Subcommand + Tail-Tokens).
///
/// # Rückgabe
/// `Ok(OpOutput { text })` mit menschlich lesbarem Bericht (Markdown-Snippets).
///
/// # Fehler
/// Siehe Modul-Doku.
#[operation(
    name = "memory",
    summary = "Long-Term-Memory: list, recall, record, stats, maintain.",
    domain = "session",
    permission = "operator",
    command(path = "/memory", visibility = "channel_parity")
)]
async fn memory(ctx: &OpContext, args: MemoryArgs) -> Result<OpOutput, OpError> {
    let store = ctx
        .service::<Arc<dyn Memory>>()
        .cloned()
        .ok_or_else(|| OpError::NotAvailable("kein Memory-Backend im Kontext".to_owned()))?;

    let sub = args.sub.as_deref().unwrap_or("list");
    match sub {
        "list" => render_hot(&*store),
        "stats" => render_stats(&*store),
        "recall" => render_recall(&*store, &args.tail),
        "record" => record(&*store, &args.tail),
        "maintain" => run_maintain(&*store),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter /memory-Subcommand: {other} (list, stats, recall, record, maintain)"
        ))),
    }
}

fn render_hot(store: &dyn Memory) -> Result<OpOutput, OpError> {
    let hot = store
        .hot()
        .map_err(|e| OpError::Execution(format!("memory.hot failed: {e}")))?;
    let text = if hot.is_empty() {
        "HOT-Tier ist leer.".to_owned()
    } else {
        format!("HOT-Tier ({} Zeilen):\n{hot}", hot.lines().count())
    };
    Ok(OpOutput { text })
}

fn render_stats(store: &dyn Memory) -> Result<OpOutput, OpError> {
    let s = store
        .stats()
        .map_err(|e| OpError::Execution(format!("memory.stats failed: {e}")))?;
    let text = format!(
        "Memory-Statistik\n\
         · HOT: {} Zeilen\n\
         · WARM: {} Namespaces / {} Zeilen\n\
         · COLD: {} Namespaces\n\
         · Offene Signale: {}",
        s.hot_lines, s.warm_namespaces, s.warm_total_lines, s.cold_namespaces, s.pending_signals,
    );
    Ok(OpOutput { text })
}

fn render_recall(store: &dyn Memory, tail: &[String]) -> Result<OpOutput, OpError> {
    if tail.is_empty() {
        return Err(OpError::InvalidArguments(
            "/memory recall <keyword…> braucht mindestens einen Suchbegriff".to_owned(),
        ));
    }
    let keywords_owned: Vec<String> = tail.to_vec();
    let keywords: Vec<&str> = keywords_owned.iter().map(String::as_str).collect();
    let query = RecallQuery {
        namespace: None,
        keywords: &keywords,
        include_cold: false,
        limit: 8,
    };
    let hits = store
        .recall(query)
        .map_err(|e| OpError::Execution(format!("memory.recall failed: {e}")))?;
    if hits.is_empty() {
        return Ok(OpOutput {
            text: format!("Keine Treffer für: {}", keywords_owned.join(" ")),
        });
    }
    let mut buf = format!("{} Treffer:\n", hits.len());
    for h in &hits {
        buf.push_str(&format!(
            "· [{}] {}\n  {}\n",
            h.tier.as_str(),
            h.namespace,
            h.content.trim()
        ));
    }
    Ok(OpOutput { text: buf })
}

fn record(store: &dyn Memory, tail: &[String]) -> Result<OpOutput, OpError> {
    let Some((kind, rest)) = tail.split_first() else {
        return Err(OpError::InvalidArguments(
            "/memory record <correction|reflection|pattern> …".to_owned(),
        ));
    };
    let signal = match kind.as_str() {
        "correction" => {
            if rest.is_empty() {
                return Err(OpError::InvalidArguments(
                    "/memory record correction <text…>".to_owned(),
                ));
            }
            Signal::Correction {
                text: rest.join(" "),
                context: None,
            }
        }
        "reflection" => {
            let joined = rest.join(" ");
            let (context, lesson) = joined.split_once("::").ok_or_else(|| {
                OpError::InvalidArguments(
                    "/memory record reflection <context> :: <lesson>".to_owned(),
                )
            })?;
            Signal::Reflection {
                context: context.trim().to_owned(),
                lesson: lesson.trim().to_owned(),
            }
        }
        "pattern" => {
            let (key, note) = rest.split_first().ok_or_else(|| {
                OpError::InvalidArguments("/memory record pattern <key> <note…>".to_owned())
            })?;
            Signal::PatternHint {
                key: (*key).clone(),
                note: note.join(" "),
            }
        }
        other => {
            return Err(OpError::InvalidArguments(format!(
                "unbekannte Signal-Art: {other} (correction, reflection, pattern)"
            )));
        }
    };
    let label = signal.kind_label();
    store
        .record(signal)
        .map_err(|e| OpError::Execution(format!("memory.record failed: {e}")))?;
    Ok(OpOutput {
        text: format!("Signal '{label}' aufgezeichnet."),
    })
}

fn run_maintain(store: &dyn Memory) -> Result<OpOutput, OpError> {
    let report = store
        .maintain()
        .map_err(|e| OpError::Execution(format!("memory.maintain failed: {e}")))?;
    Ok(OpOutput {
        text: format!(
            "Maintenance abgeschlossen: {} Signale, {} promoted→HOT, {} demoted→COLD, {} neue WARM.",
            report.signals_processed,
            report.promoted_to_hot,
            report.demoted_to_cold,
            report.warm_created,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{MemoryArgs, MemoryOperation};
    use crate::testutil::toks;
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, Operation};

    #[test]
    fn from_raw_args_no_tokens_uses_default_sub() {
        let a = MemoryArgs::from_raw_args(&toks(&[])).unwrap();
        assert!(a.sub.is_none());
        assert!(a.tail.is_empty());
    }

    #[test]
    fn from_raw_args_captures_sub_and_tail() {
        let a = MemoryArgs::from_raw_args(&toks(&["recall", "popup", "enter"])).unwrap();
        assert_eq!(a.sub.as_deref(), Some("recall"));
        assert_eq!(a.tail, vec!["popup".to_owned(), "enter".to_owned()]);
    }

    #[test]
    fn from_raw_args_maintain_has_no_tail() {
        let a = MemoryArgs::from_raw_args(&toks(&["maintain"])).unwrap();
        assert_eq!(a.sub.as_deref(), Some("maintain"));
        assert!(a.tail.is_empty());
    }

    #[test]
    fn memory_operation_is_command_only() {
        let surfaces = &MemoryOperation.meta().surfaces;

        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::Command {
                    path: "/memory",
                    visibility: CommandVisibility::ChannelParity,
                }
            )
        }));
    }
}
