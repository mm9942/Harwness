//! Schreibt den Kontext-Ledger eines Turns (siehe `harw-context-ledger`).
//!
//! Läuft in `build_request` nach der Montage: jedes gesammelte Fragment ist
//! entweder angeboten oder (laut [`ContextAssembly`]) ausgelassen. Es werden
//! nur Label, Anbieter, Vertrauensklasse und Größen aufgezeichnet — nie der
//! Inhalt.

use std::collections::HashSet;

use harw_context::Fragment;
use harw_context_ledger::{LedgerEntry, LedgerKind, LedgerSink};

use crate::context_budget::ContextAssembly;

/// Label, Anbieter, Vertrauensklasse (Text), Bytes und Tokens eines Fragments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FragmentFacts {
    label: String,
    provider: String,
    trust: &'static str,
    bytes: u64,
    tokens: u32,
}

/// Merkt sich die Fakten der gesammelten Fragmente, bevor die Montage sie
/// verbraucht.
pub(crate) fn snapshot(fragments: &[Fragment]) -> Vec<FragmentFacts> {
    fragments
        .iter()
        .map(|fragment| FragmentFacts {
            label: fragment.label.as_str().to_owned(),
            provider: fragment.origin.provider.clone(),
            trust: match fragment.trust {
                harw_context::TrustClass::Instruction => "instruction",
                harw_context::TrustClass::Evidence => "evidence",
                harw_context::TrustClass::Data => "data",
            },
            bytes: u64::try_from(fragment.body.len()).unwrap_or(u64::MAX),
            tokens: fragment.cost.0,
        })
        .collect()
}

/// Baut die Ledger-Einträge eines Turns: je Fragment `Offered` oder
/// `Omitted`; ohne Auslassungsliste gelten alle als angeboten.
pub(crate) fn entries_for_turn(
    session_id: &str,
    turn_id: &str,
    facts: &[FragmentFacts],
    assembly: &ContextAssembly,
) -> Vec<LedgerEntry> {
    let omitted: HashSet<&str> = assembly
        .omitted_fragment_labels
        .iter()
        .map(String::as_str)
        .collect();
    facts
        .iter()
        .map(|fact| {
            let is_omitted = omitted.contains(fact.label.as_str());
            let kind = if is_omitted {
                LedgerKind::Omitted
            } else {
                LedgerKind::Offered
            };
            let mut entry = LedgerEntry::new(session_id, turn_id, kind, fact.label.clone());
            entry.provider = Some(fact.provider.clone());
            entry.trust = Some(fact.trust.to_owned());
            entry.bytes = Some(fact.bytes);
            entry.tokens = Some(fact.tokens);
            if is_omitted {
                entry.reason = Some("omitted-by-assembly".to_owned());
            }
            entry
        })
        .collect()
}

/// Schreibt die Einträge eines Turns in `sink` (leere Turns schreiben nichts).
pub(crate) fn record_turn(
    sink: &dyn LedgerSink,
    session_id: &str,
    turn_id: &str,
    facts: &[FragmentFacts],
    assembly: &ContextAssembly,
) {
    let entries = entries_for_turn(session_id, turn_id, facts, assembly);
    if !entries.is_empty() {
        sink.record(&entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_context_ledger::MemoryLedger;

    fn facts(label: &str) -> FragmentFacts {
        FragmentFacts {
            label: label.to_owned(),
            provider: "memory".to_owned(),
            trust: "data",
            bytes: 10,
            tokens: 3,
        }
    }

    #[test]
    fn omitted_labels_become_omitted_entries_and_the_rest_offered() {
        let assembly = ContextAssembly {
            included_fragment_labels: vec!["a".to_owned()],
            omitted_fragment_labels: vec!["b".to_owned()],
            ..ContextAssembly::default()
        };
        let ledger = MemoryLedger::new();
        record_turn(&ledger, "s", "t", &[facts("a"), facts("b")], &assembly);
        let all = ledger.entries();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].kind, LedgerKind::Offered);
        assert_eq!(all[1].kind, LedgerKind::Omitted);
        assert_eq!(all[1].reason.as_deref(), Some("omitted-by-assembly"));
        assert_eq!(all[0].trust.as_deref(), Some("data"));
    }

    #[test]
    fn an_empty_turn_writes_nothing() {
        let ledger = MemoryLedger::new();
        record_turn(&ledger, "s", "t", &[], &ContextAssembly::default());
        assert!(ledger.entries().is_empty());
    }
}
