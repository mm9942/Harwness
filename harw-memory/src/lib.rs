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
//!   docs/design/memory-promotion.md`); bewusst nicht in `FileMemoryStore::maintain`
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
pub mod consolidation;
pub mod context_policy;
pub mod context_provider;
pub mod context_selector;
pub mod contradiction_index;
pub mod detect;
pub mod epistemic;
pub mod error;
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
pub use promote::{
    PromotionCandidate, PromotionOutcome, SignalOccurrence, apply_promotion, evaluate_signals,
};
pub use store::Memory;
pub use types::{Entry, MaintenanceReport, RecallQuery, Signal, Stats, Tier};
pub use workflow::{WorkflowMarker, WorkflowStep};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-memory-{}-{}-{}", tag, std::process::id(), id));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn open_creates_subdirs() -> TestResult {
        let root = tmp_root("open");
        let _store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        for sub in ["warm", "cold", "signals"] {
            assert!(root.join(sub).is_dir(), "expected {sub}/ under root");
        }
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn hot_is_empty_when_no_file() -> TestResult {
        let root = tmp_root("hot-empty");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        assert_eq!(store.hot().map_err(ctx("hot"))?, "");
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn hot_reads_content_when_present() -> TestResult {
        let root = tmp_root("hot-present");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        std::fs::write(root.join("HOT.md"), "Regel 1: keine Doppelantworten.\n")
            .map_err(ctx("HOT.md schreiben"))?;
        assert_eq!(
            store.hot().map_err(ctx("hot"))?,
            "Regel 1: keine Doppelantworten.\n"
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn hot_overflow_is_rejected() -> TestResult {
        let root = tmp_root("hot-overflow");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        let body: String = (0..200).map(|i| format!("Zeile {i}\n")).collect();
        std::fs::write(root.join("HOT.md"), body).map_err(ctx("HOT.md schreiben"))?;
        let Err(err) = store.hot() else {
            return Err(TestError::Unexpected("hot_overflow: Err erwartet".into()));
        };
        assert!(matches!(err, MemoryError::TierOverflow { .. }));
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn record_appends_to_signal_log() -> TestResult {
        let root = tmp_root("record");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        store
            .record(Signal::Correction {
                text: "nicht bevormunden".into(),
                context: Some("session-42".into()),
            })
            .map_err(ctx("record 1"))?;
        store
            .record(Signal::Correction {
                text: "keine Emojis".into(),
                context: None,
            })
            .map_err(ctx("record 2"))?;
        let content = std::fs::read_to_string(root.join("signals/corrections.jsonl"))
            .map_err(ctx("corrections.jsonl lesen"))?;
        assert_eq!(content.lines().count(), 2);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn recall_filters_by_namespace_and_keywords() -> TestResult {
        let root = tmp_root("recall");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        std::fs::create_dir_all(root.join("warm/project")).map_err(ctx("warm/project anlegen"))?;
        std::fs::write(
            root.join("warm/project/harwness.md"),
            "TUI-Fix: Enter darf Popup nicht blockieren.\n",
        )
        .map_err(ctx("harwness.md schreiben"))?;
        std::fs::write(
            root.join("warm/domain-rust.md"),
            "Nutze `?` statt `unwrap()` in Prod-Pfaden.\n",
        )
        .map_err(ctx("domain-rust.md schreiben"))?;
        let hits = store
            .recall(RecallQuery {
                namespace: Some("project/harwness"),
                keywords: &["Popup"],
                include_cold: false,
                limit: 10,
            })
            .map_err(ctx("recall"))?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].namespace, "project/harwness");
        assert_eq!(hits[0].tier, Tier::Warm);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn maintain_runs_state_machine_to_db_committed() -> TestResult {
        let root = tmp_root("maintain");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        store
            .record(Signal::Reflection {
                context: "layer".into(),
                lesson: "Bridge-Crate für Cross-Crate-Getter.".into(),
            })
            .map_err(ctx("record"))?;
        let report = store.maintain().map_err(ctx("maintain"))?;
        assert_eq!(report.signals_processed, 1);
        let bytes =
            std::fs::read(root.join("workflow.json")).map_err(ctx("workflow.json lesen"))?;
        let marker: WorkflowMarker =
            serde_json::from_slice(&bytes).map_err(ctx("workflow.json parsen"))?;
        assert_eq!(marker.step, WorkflowStep::DbCommitted);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn maintain_is_idempotent_across_runs() -> TestResult {
        let root = tmp_root("idempotent");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        store
            .record(Signal::PatternHint {
                key: "greeting".into(),
                note: "User grüßt konsistent mit 'moin'.".into(),
            })
            .map_err(ctx("record"))?;
        let first = store.maintain().map_err(ctx("maintain 1"))?;
        assert_eq!(first.signals_processed, 1);
        let second = store.maintain().map_err(ctx("maintain 2"))?;
        assert_eq!(
            second.signals_processed, 0,
            "already-seen signals must not double-count"
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn stats_reports_counts() -> TestResult {
        let root = tmp_root("stats");
        let store = FileMemoryStore::open(&root).map_err(ctx("open"))?;
        std::fs::write(root.join("HOT.md"), "Regel A\nRegel B\n")
            .map_err(ctx("HOT.md schreiben"))?;
        std::fs::write(root.join("warm/foo.md"), "x\ny\nz\n")
            .map_err(ctx("warm/foo.md schreiben"))?;
        let s = store.stats().map_err(ctx("stats"))?;
        assert_eq!(s.hot_lines, 2);
        assert_eq!(s.warm_namespaces, 1);
        assert_eq!(s.warm_total_lines, 3);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
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

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
