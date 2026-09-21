//! `harw-memory` — persistente, tokensparsame Memory-Schicht für die Harness.
//!
//! # Verantwortungsbereich
//! Dieses Crate stellt eine dateibasierte Standard-Memory bereit, die pro
//! Session weiterläuft und aus expliziten Signalen (Korrektur, Reflexion,
//! Muster-Kandidaten) lernt. Es folgt der Design-Doc unter
//! `docs/design/harw-memory.md`.
//!
//! # Schlüsseltypen
//! - [`store::Memory`] — Backend-agnostischer Trait.
//! - [`file_store::FileMemoryStore`] — Standard-Implementierung.
//! - [`types::Signal`], [`types::Tier`], [`types::Entry`], [`types::RecallQuery`].
//! - [`workflow::WorkflowStep`], [`workflow::WorkflowMarker`] — persistente
//!   Workflow-State-Machine für idempotente `maintain()`-Läufe.
//! - [`promote::evaluate_signals`], [`promote::apply_promotion`] — gefensterte
//!   Signal-Promotion-Auswertung (Track A, `docs/design/
//!   track-a-memory-promotion.md`); bewusst nicht in `FileMemoryStore::maintain`
//!   verdrahtet, siehe die Moduldoku von [`promote`].
//!
//! # Nebenläufigkeit
//! `FileMemoryStore` ist `Send + Sync`. `record()` und `maintain()` serialisieren
//! intern; Reads sind lock-frei und sehen den zuletzt committeten Zustand.
//!
//! # Fehler
//! - [`error::MemoryError`] — I/O, Serde, Tier-Overflow, invalider Namespace,
//!   Lock-Contention.
//!
//! # Quellenbindung (AW3-03)
//! [`context_provider::MemoryContextProvider`] übersetzt HOT/STM/WARM in
//! [`harw_context::Fragment`]e für die Montage; [`context_provider::selection_role_for`]
//! ist die eine Naht, die `TurnInputContext::metadata` in eine
//! [`context_selector::SelectionRole`] übersetzt — siehe die Moduldoku von
//! [`context_provider`] für die Begründung, warum es genau diese eine
//! Funktion sein muss.
//!
//! # Beispiel
//! ```no_run
//! use harw_memory::file_store::FileMemoryStore;
//! use harw_memory::store::Memory;
//! use harw_memory::types::Signal;
//!
//! let store = FileMemoryStore::open("/tmp/harw-mem-example").unwrap();
//! store
//!     .record(Signal::Reflection {
//!         context: "TUI-Command-Wiring".into(),
//!         lesson: "Popup darf Enter nie schlucken.".into(),
//!     })
//!     .unwrap();
//! let hot = store.hot().unwrap();
//! assert!(hot.is_empty() || hot.lines().count() <= 100);
//! ```

pub mod capture;
pub mod context_policy;
pub mod context_provider;
pub mod context_selector;
pub mod contradiction_index;
pub mod detect;
pub mod epistemic;
pub mod error;
pub mod consolidation;
pub mod extraction;
pub mod facts;
pub mod file_index;
pub mod file_store;
pub mod heartbeat;
pub mod learning;
pub mod outcome_tracker;
pub mod promote;
pub mod short_term;
pub mod store;
pub mod summary;
pub mod types;
pub mod workflow;

pub use capture::{ProjectMemoryCapture, consolidate_project_memories};
pub use context_provider::{
    MEMORY_CONTEXT_MAX_TRUST, MEMORY_CONTEXT_MAY_CARRY_USER_CONTENT, MEMORY_CONTEXT_NAMESPACE,
    MemoryContextProvider, selection_role_for,
};
pub use detect::detect_correction;
pub use error::{MemoryError, MemoryResult};
pub use extraction::{
    EntryRole, ExtractionCandidate, ExtractionError, ExtractionPolicy, IncomingStore,
    TranscriptEntry, build_input, parse_response, select_sessions, system_prompt, user_prompt,
};
pub use facts::{Fact, FactScope, FactStore, FactType, redact, slugify};
pub use file_index::{FileKnowledge, FileKnowledgeIndex};
pub use file_store::FileMemoryStore;
pub use promote::{PromotionCandidate, PromotionOutcome, SignalOccurrence, apply_promotion, evaluate_signals};
pub use store::Memory;
pub use types::{Entry, MaintenanceReport, RecallQuery, Signal, Stats, Tier};
pub use workflow::{WorkflowMarker, WorkflowStep};

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-memory-{}-{}-{}", tag, std::process::id(), id));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn open_creates_subdirs() {
        let root = tmp_root("open");
        let _store = FileMemoryStore::open(&root).unwrap();
        for sub in ["warm", "cold", "signals"] {
            assert!(root.join(sub).is_dir(), "expected {sub}/ under root");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hot_is_empty_when_no_file() {
        let root = tmp_root("hot-empty");
        let store = FileMemoryStore::open(&root).unwrap();
        assert_eq!(store.hot().unwrap(), "");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hot_reads_content_when_present() {
        let root = tmp_root("hot-present");
        let store = FileMemoryStore::open(&root).unwrap();
        std::fs::write(root.join("HOT.md"), "Regel 1: keine Doppelantworten.\n").unwrap();
        assert_eq!(store.hot().unwrap(), "Regel 1: keine Doppelantworten.\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hot_overflow_is_rejected() {
        let root = tmp_root("hot-overflow");
        let store = FileMemoryStore::open(&root).unwrap();
        let body: String = (0..200).map(|i| format!("Zeile {i}\n")).collect();
        std::fs::write(root.join("HOT.md"), body).unwrap();
        assert!(matches!(
            store.hot().unwrap_err(),
            MemoryError::TierOverflow { .. }
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn record_appends_to_signal_log() {
        let root = tmp_root("record");
        let store = FileMemoryStore::open(&root).unwrap();
        store
            .record(Signal::Correction {
                text: "nicht bevormunden".into(),
                context: Some("session-42".into()),
            })
            .unwrap();
        store
            .record(Signal::Correction {
                text: "keine Emojis".into(),
                context: None,
            })
            .unwrap();
        let content = std::fs::read_to_string(root.join("signals/corrections.jsonl")).unwrap();
        assert_eq!(content.lines().count(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recall_filters_by_namespace_and_keywords() {
        let root = tmp_root("recall");
        let store = FileMemoryStore::open(&root).unwrap();
        std::fs::create_dir_all(root.join("warm/project")).unwrap();
        std::fs::write(
            root.join("warm/project/harwness.md"),
            "TUI-Fix: Enter darf Popup nicht blockieren.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("warm/domain-rust.md"),
            "Nutze `?` statt `unwrap()` in Prod-Pfaden.\n",
        )
        .unwrap();
        let hits = store
            .recall(RecallQuery {
                namespace: Some("project/harwness"),
                keywords: &["Popup"],
                include_cold: false,
                limit: 10,
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].namespace, "project/harwness");
        assert_eq!(hits[0].tier, Tier::Warm);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn maintain_runs_state_machine_to_db_committed() {
        let root = tmp_root("maintain");
        let store = FileMemoryStore::open(&root).unwrap();
        store
            .record(Signal::Reflection {
                context: "layer".into(),
                lesson: "Bridge-Crate für Cross-Crate-Getter.".into(),
            })
            .unwrap();
        let report = store.maintain().unwrap();
        assert_eq!(report.signals_processed, 1);
        let bytes = std::fs::read(root.join("workflow.json")).unwrap();
        let marker: WorkflowMarker = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(marker.step, WorkflowStep::DbCommitted);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn maintain_is_idempotent_across_runs() {
        let root = tmp_root("idempotent");
        let store = FileMemoryStore::open(&root).unwrap();
        store
            .record(Signal::PatternHint {
                key: "greeting".into(),
                note: "User grüßt konsistent mit 'moin'.".into(),
            })
            .unwrap();
        let first = store.maintain().unwrap();
        assert_eq!(first.signals_processed, 1);
        let second = store.maintain().unwrap();
        assert_eq!(
            second.signals_processed, 0,
            "already-seen signals must not double-count"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stats_reports_counts() {
        let root = tmp_root("stats");
        let store = FileMemoryStore::open(&root).unwrap();
        std::fs::write(root.join("HOT.md"), "Regel A\nRegel B\n").unwrap();
        std::fs::write(root.join("warm/foo.md"), "x\ny\nz\n").unwrap();
        let s = store.stats().unwrap();
        assert_eq!(s.hot_lines, 2);
        assert_eq!(s.warm_namespaces, 1);
        assert_eq!(s.warm_total_lines, 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn workflow_step_next_terminates_at_db_committed() {
        assert_eq!(WorkflowStep::Idle.next(), Some(WorkflowStep::Claimed));
        assert_eq!(WorkflowStep::DbCommitted.next(), None);
    }

    #[test]
    fn store_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FileMemoryStore>();
    }
}
