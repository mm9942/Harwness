//! Rollout-Summary-Extraktion — Komprimierung von STM-Sequenzen zu promotablen Reflexionen.
//!
//! # Verantwortungsbereich
//! Transformiert [`crate::short_term::StmEntry`]-Sequenzen in kompakte
//! [`crate::types::Signal::Reflection`]-Einträge, die in die Memory-Promotion-Pipeline
//! eingespeist werden. Reine Datentransformation — kein I/O außer dem abschließenden
//! `store.record`-Aufruf in [`extract_and_record`].
//!
//! Folgt `docs/design/memory-v2.md` §3 (STM) und §5 (Signals) sowie
//! `philosophy.md` §4 („Memory-Konsolidierung ist ein langlebiger Workflow") und
//! §16 Invariante 8 („Memory ist Promotion-Pipeline").
//!
//! # Schlüsseltypen
//! - [`RolloutSummary`] — Zusammenfassung einer abgeschlossenen Turn-Sequenz.
//! - [`SummaryConfig`] — Schwellenwerte für Mindest-Einträge, Mindest-Salience und Lesson-Länge.
//! - [`extract_summary`] — reine, infallible Datentransformation.
//! - [`extract_and_record`] — Extraktion + Persistenz als `Signal::Reflection`.
//!
//! # Nebenläufigkeit
//! `extract_summary` ist rein funktional und threadunsicher nur durch Mut-Borrow auf
//! der internen Sortiertabelle — nach außen hin unproblematisch. `extract_and_record`
//! delegiert I/O an die [`crate::store::Memory`]-Implementierung, die `Send + Sync` ist.
//!
//! # Fehler
//! `extract_summary` ist infallibel. `extract_and_record` propagiert
//! [`crate::error::MemoryError`] aus `store.record()`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_memory::summary::{extract_summary, SummaryConfig};
//! use harw_memory::short_term::{StmEntry, StmRole};
//! use time::OffsetDateTime;
//!
//! let entries = vec![
//!     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::Assistant, salience: 80, content: "Lektion A".to_owned() },
//!     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::Tool,      salience: 75, content: "Lektion B".to_owned() },
//!     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::User,      salience: 50, content: "Frage des Users".to_owned() },
//! ];
//! let summary = extract_summary(&entries, "coding-task", SummaryConfig::default());
//! assert!(summary.is_some());
//! ```

use serde::{Deserialize, Serialize};

use crate::short_term::{StmEntry, StmRole};

// ── Typen ────────────────────────────────────────────────────────────────────

/// Zusammenfassung einer abgeschlossenen Turn-Sequenz.
///
/// # Beschreibung
/// Repräsentiert das komprimierte Ergebnis einer STM-Snapshot-Auswertung.
/// Wird von [`extract_summary`] erzeugt und von [`extract_and_record`] als
/// [`crate::types::Signal::Reflection`] in die Promotion-Pipeline eingespeist.
///
/// Folgt `docs/design/memory-v2.md` §5 (Signals — Reflection).
///
/// # Felder
/// - `context` (`String`): Kontext-Label der Aufgabe (z. B. `"coding-task"`, `"chat"`).
/// - `lesson` (`String`): Extrahierte Lektion; maximal [`SummaryConfig::max_lesson_chars`] Zeichen.
/// - `source_entries` (`usize`): Anzahl der Quell-Einträge, aus denen zusammengefasst wurde.
/// - `total_salience` (`u32`): Summe der Salience-Werte aller Quell-Einträge.
///
/// # Beispiel
/// ```rust
/// use harw_memory::summary::RolloutSummary;
///
/// let s = RolloutSummary {
///     context: "coding-task".to_owned(),
///     lesson: "Keine unwrap() in Prod-Pfaden.".to_owned(),
///     source_entries: 5,
///     total_salience: 320,
/// };
/// assert_eq!(s.source_entries, 5);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolloutSummary {
    /// Kontext-Label (z. B. `"coding-task"`, `"chat"`, `"verification"`).
    pub context: String,
    /// Extrahierte Lektion (Freitext, max [`SummaryConfig::max_lesson_chars`] Zeichen).
    pub lesson: String,
    /// Anzahl der Einträge, aus denen zusammengefasst wurde.
    pub source_entries: usize,
    /// Summe der Salience-Werte der Quelle.
    pub total_salience: u32,
}

/// Konfiguration der Summary-Extraktion.
///
/// # Beschreibung
/// Steuert die drei Schwellenwerte, die erfüllt sein müssen, damit eine
/// [`RolloutSummary`] erzeugt wird. Folgt `docs/design/memory-v2.md` §3
/// (Mindestvolumen) und §5 (Signalqualität).
///
/// # Felder
/// - `min_entries` (`usize`): Mindestanzahl an Einträgen; Standard: `3`.
/// - `min_total_salience` (`u32`): Mindest-Salience-Summe; Standard: `60`.
/// - `max_lesson_chars` (`usize`): Maximale Lesson-Länge in Unicode-Zeichen; Standard: `400`.
///
/// # Beispiel
/// ```rust
/// use harw_memory::summary::SummaryConfig;
///
/// let cfg = SummaryConfig::default();
/// assert_eq!(cfg.min_entries, 3);
/// assert_eq!(cfg.min_total_salience, 60);
/// assert_eq!(cfg.max_lesson_chars, 400);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryConfig {
    /// Mindestanzahl an Einträgen, bevor eine Summary gebildet wird.
    pub min_entries: usize,
    /// Mindest-Salience-Summe der Quelle.
    pub min_total_salience: u32,
    /// Maximale Lesson-Länge in Zeichen; über diesem Wert wird gekürzt.
    pub max_lesson_chars: usize,
}

impl Default for SummaryConfig {
    /// Gibt die Design-Doc-konformen Standardwerte zurück.
    ///
    /// # Rückgabe
    /// `SummaryConfig { min_entries: 3, min_total_salience: 60, max_lesson_chars: 400 }`
    fn default() -> Self {
        Self {
            min_entries: 3,
            min_total_salience: 60,
            max_lesson_chars: 400,
        }
    }
}

// ── Funktionen ───────────────────────────────────────────────────────────────

/// Bildet aus einer STM-Snapshot-Liste eine Summary — falls die Schwellen erreicht sind.
///
/// # Beschreibung
/// Führt die salience-basierte Verdichtung nach `docs/design/memory-v2.md` §3 durch:
///
/// 1. Prüft `entries.len() >= cfg.min_entries`; sonst `None`.
/// 2. Berechnet `total_salience`; wenn `< cfg.min_total_salience` → `None`.
/// 3. Filtert Einträge auf [`StmRole::Assistant`] und [`StmRole::Tool`];
///    fällt auf alle Rollen zurück, wenn der Filter leer ist.
/// 4. Sortiert absteigend nach `salience`, bei Gleichstand aufsteigend nach `at`.
/// 5. Nimmt bis zu 5 Top-Einträge und konkateniert `content` mit `"; "`.
/// 6. Kürzt auf `cfg.max_lesson_chars` Unicode-Zeichen.
///
/// # Argumente
/// - `entries` (`&[StmEntry]`): Snapshot-Slice aus [`crate::short_term::ShortTermMemory::snapshot`].
/// - `context` (`impl Into<String>`): Kontext-Label der laufenden Aufgabe.
/// - `cfg` (`SummaryConfig`): Schwellenwerte und Längen-Limit.
///
/// # Rückgabe
/// `Some(RolloutSummary)` wenn alle Schwellen erfüllt sind; `None` sonst.
///
/// # Panics
/// Keine.
///
/// # Concurrency
/// Rein funktional, keine Locks oder Shared State.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::summary::{extract_summary, SummaryConfig};
/// use harw_memory::short_term::{StmEntry, StmRole};
/// use time::OffsetDateTime;
///
/// let entries = vec![
///     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::Assistant, salience: 90, content: "A".to_owned() },
///     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::Tool, salience: 80, content: "B".to_owned() },
///     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::User, salience: 50, content: "C".to_owned() },
/// ];
/// let s = extract_summary(&entries, "test", SummaryConfig::default());
/// assert!(s.is_some());
/// ```
#[must_use]
pub fn extract_summary(
    entries: &[StmEntry],
    context: impl Into<String>,
    cfg: SummaryConfig,
) -> Option<RolloutSummary> {
    // Schwelle 1: Mindestanzahl an Einträgen.
    if entries.len() < cfg.min_entries {
        return None;
    }

    // Schwelle 2: Mindest-Salience-Summe.
    let total_salience: u32 = entries.iter().map(|e| u32::from(e.salience)).sum();
    if total_salience < cfg.min_total_salience {
        return None;
    }

    // Rollenfilterung: bevorzuge Assistant und Tool.
    let mut filtered: Vec<&StmEntry> = entries
        .iter()
        .filter(|e| matches!(e.role, StmRole::Assistant | StmRole::Tool))
        .collect();

    // Fallback: alle Rollen, wenn kein Assistant/Tool vorhanden.
    if filtered.is_empty() {
        filtered = entries.iter().collect();
    }

    // Sortierung: Salience absteigend, bei Gleichstand at aufsteigend.
    filtered.sort_by(|a, b| b.salience.cmp(&a.salience).then_with(|| a.at.cmp(&b.at)));

    // Top 5 auswählen und Lesson konkatenieren.
    let lesson_raw: String = filtered
        .iter()
        .take(5)
        .map(|e| e.content.as_str())
        .collect::<Vec<_>>()
        .join("; ");

    // Auf max_lesson_chars Zeichen kürzen.
    let lesson: String = lesson_raw.chars().take(cfg.max_lesson_chars).collect();

    Some(RolloutSummary {
        context: context.into(),
        lesson,
        source_entries: entries.len(),
        total_salience,
    })
}

/// Führt [`extract_summary`] aus und persistiert das Ergebnis als
/// [`crate::types::Signal::Reflection`] via `store.record`.
///
/// # Beschreibung
/// Bildet den Übergang zwischen reiner Datentransformation und der Persistenz-Schicht.
/// Folgt `philosophy.md` §4 und §16 Invariante 8: nur promotable Summaries werden
/// in die Pipeline eingespeist.
///
/// # Argumente
/// - `store` (`&M`): Memory-Backend (`M: Memory`); muss `Send + Sync` sein.
/// - `entries` (`&[StmEntry]`): Snapshot aus [`crate::short_term::ShortTermMemory::snapshot`].
/// - `context` (`impl Into<String>`): Kontext-Label der laufenden Aufgabe.
/// - `cfg` (`SummaryConfig`): Schwellenwerte und Längen-Limit.
///
/// # Rückgabe
/// - `Ok(Some(summary))` — Summary gebildet und persistiert.
/// - `Ok(None)` — Schwellen nicht erreicht; kein Signal geschrieben.
///
/// # Fehler
/// - [`crate::error::MemoryError::Io`] — Schreiben der Signal-Datei fehlgeschlagen.
/// - [`crate::error::MemoryError::Serde`] — JSON-Serialisierung fehlgeschlagen.
///
/// # Panics
/// Keine.
///
/// # Concurrency
/// Threadsicher durch die `Send + Sync`-Garantie von `M`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::summary::{extract_and_record, SummaryConfig};
/// use harw_memory::short_term::{StmEntry, StmRole};
/// use harw_memory::file_store::FileMemoryStore;
/// use time::OffsetDateTime;
///
/// let store = FileMemoryStore::open("/tmp/harw-summary-example").unwrap();
/// let entries = vec![
///     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::Assistant, salience: 90, content: "Lektion".to_owned() },
///     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::Tool, salience: 80, content: "Ergebnis".to_owned() },
///     StmEntry { at: OffsetDateTime::now_utc(), role: StmRole::User, salience: 60, content: "Frage".to_owned() },
/// ];
/// let result = extract_and_record(&store, &entries, "coding-task", SummaryConfig::default()).unwrap();
/// assert!(result.is_some());
/// ```
pub fn extract_and_record<M: crate::store::Memory>(
    store: &M,
    entries: &[StmEntry],
    context: impl Into<String>,
    cfg: SummaryConfig,
) -> crate::error::MemoryResult<Option<RolloutSummary>> {
    match extract_summary(entries, context, cfg) {
        Some(s) => {
            let signal = crate::types::Signal::Reflection {
                context: s.context.clone(),
                lesson: s.lesson.clone(),
            };
            store.record(signal)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use time::OffsetDateTime;

    use super::*;
    use crate::short_term::StmRole;

    /// Erzeugt einen einzigartigen temporären Verzeichnispfad.
    fn tmp_root(tag: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "harw-summary-{}-{}-{}",
            tag,
            std::process::id(),
            id
        ))
    }

    /// Hilfsfunktion: erzeugt einen `StmEntry` mit gegebenen Werten.
    fn entry(role: StmRole, salience: u8, content: &str) -> StmEntry {
        StmEntry {
            at: OffsetDateTime::now_utc(),
            role,
            salience,
            content: content.to_owned(),
        }
    }

    // ── Test 1 ───────────────────────────────────────────────────────────────

    /// Zu wenige Einträge (2 von min=3) → None.
    #[test]
    fn too_few_entries_returns_none() {
        let entries = vec![
            entry(StmRole::Assistant, 90, "Lektion A"),
            entry(StmRole::Tool, 80, "Lektion B"),
        ];
        let cfg = SummaryConfig {
            min_entries: 3,
            min_total_salience: 60,
            max_lesson_chars: 400,
        };
        let result = extract_summary(&entries, "test", cfg);
        assert!(
            result.is_none(),
            "2 Einträge bei min_entries=3 muss None ergeben"
        );
    }

    // ── Test 2 ───────────────────────────────────────────────────────────────

    /// 5 Einträge à Salience 5 → total 25 < 60 → None.
    #[test]
    fn low_salience_returns_none() {
        let entries: Vec<StmEntry> = (0..5)
            .map(|i| entry(StmRole::User, 5, &format!("msg {i}")))
            .collect();
        let cfg = SummaryConfig::default(); // min_total_salience = 60
        let result = extract_summary(&entries, "test", cfg);
        assert!(result.is_none(), "total_salience=25 < 60 muss None ergeben");
    }

    // ── Test 3 ───────────────────────────────────────────────────────────────

    /// 5 Einträge mit hoher Salience → Some(summary), source_entries=5.
    #[test]
    fn sufficient_input_produces_summary() {
        let entries: Vec<StmEntry> = (0..5)
            .map(|i| entry(StmRole::Assistant, 80, &format!("lektion {i}")))
            .collect();
        let cfg = SummaryConfig::default();
        let result = extract_summary(&entries, "coding-task", cfg);
        let summary = result.expect("5 Einträge mit hoher Salience müssen Some ergeben");
        assert_eq!(summary.source_entries, 5, "source_entries muss 5 sein");
        assert_eq!(summary.context, "coding-task");
        assert!(
            summary.total_salience >= 60,
            "total_salience muss >= 60 sein"
        );
        assert!(!summary.lesson.is_empty(), "lesson darf nicht leer sein");
    }

    // ── Test 4 ───────────────────────────────────────────────────────────────

    /// Mix User+Assistant+Tool: nur Assistant/Tool-Content im Lesson-Text.
    #[test]
    fn prefers_assistant_and_tool() {
        let mut entries = vec![
            entry(StmRole::User, 10, "user-data"),
            entry(StmRole::User, 10, "user-data"),
            entry(StmRole::User, 10, "user-data"),
        ];
        // Assistant und Tool mit hoher Salience.
        entries.push(entry(StmRole::Assistant, 90, "assistant-lektion"));
        entries.push(entry(StmRole::Tool, 85, "tool-ergebnis"));
        entries.push(entry(StmRole::Assistant, 80, "assistant-notiz"));

        let cfg = SummaryConfig::default();
        let result = extract_summary(&entries, "mixed-test", cfg);
        let summary = result.expect("Ausreichend Einträge mit hoher Salience");

        assert!(
            summary.lesson.contains("assistant-lektion")
                || summary.lesson.contains("tool-ergebnis"),
            "Lesson muss Assistant- oder Tool-Content enthalten; got: {:?}",
            summary.lesson
        );
        assert!(
            !summary.lesson.contains("user-data"),
            "Lesson darf keinen User-Content enthalten, wenn Assistant/Tool verfügbar; got: {:?}",
            summary.lesson
        );
    }

    // ── Test 5 ───────────────────────────────────────────────────────────────

    /// Nur User-Einträge → Fallback auf alle Rollen, Summary vorhanden.
    #[test]
    fn falls_back_when_only_user_role() {
        let entries: Vec<StmEntry> = (0..5)
            .map(|i| entry(StmRole::User, 20, &format!("user-content-{i}")))
            .collect();
        let cfg = SummaryConfig {
            min_entries: 3,
            min_total_salience: 60,
            max_lesson_chars: 400,
        };
        let result = extract_summary(&entries, "user-only", cfg);
        let summary = result.expect("Fallback auf User-Einträge muss Summary ergeben");
        assert!(
            summary.lesson.contains("user-content"),
            "User-Content muss im Fallback-Fall in der Lesson erscheinen; got: {:?}",
            summary.lesson
        );
    }

    // ── Test 6 ───────────────────────────────────────────────────────────────

    /// Sehr langer Content, max_lesson_chars=50 → lesson.chars().count() <= 50.
    #[test]
    fn lesson_truncated_to_max_chars() {
        let long_text = "x".repeat(300);
        let entries = vec![
            entry(StmRole::Assistant, 90, &long_text),
            entry(StmRole::Tool, 80, &long_text),
            entry(StmRole::User, 70, &long_text),
        ];
        let cfg = SummaryConfig {
            min_entries: 3,
            min_total_salience: 60,
            max_lesson_chars: 50,
        };
        let result = extract_summary(&entries, "truncation-test", cfg);
        let summary = result.expect("Ausreichend Einträge");
        let char_count = summary.lesson.chars().count();
        assert!(
            char_count <= 50,
            "Lesson muss auf 50 Zeichen gekürzt sein; tatsächlich: {char_count}"
        );
    }

    // ── Test 7 ───────────────────────────────────────────────────────────────

    /// `extract_and_record` persistiert eine Reflection in `signals/reflections.jsonl`.
    #[test]
    fn extract_and_record_persists_reflection() {
        let root = tmp_root("record");
        let store = crate::file_store::FileMemoryStore::open(&root)
            .expect("FileMemoryStore::open muss erfolgreich sein");

        let entries = vec![
            entry(StmRole::Assistant, 90, "Lesson 1"),
            entry(StmRole::Tool, 80, "Lesson 2"),
            entry(StmRole::User, 70, "Frage"),
        ];
        let cfg = SummaryConfig::default();
        let result = extract_and_record(&store, &entries, "record-test", cfg)
            .expect("extract_and_record darf keinen Fehler erzeugen");

        assert!(result.is_some(), "Summary muss erzeugt worden sein");

        let jsonl_path = root.join("signals/reflections.jsonl");
        let content = std::fs::read_to_string(&jsonl_path)
            .expect("reflections.jsonl muss nach record existieren");
        assert_eq!(
            content.lines().count(),
            1,
            "reflections.jsonl muss genau eine Zeile enthalten"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 8 ───────────────────────────────────────────────────────────────

    /// `SummaryConfig::default()` entspricht den Design-Doc-Werten {3, 60, 400}.
    #[test]
    fn default_config_matches_design() {
        let cfg = SummaryConfig::default();
        assert_eq!(cfg.min_entries, 3, "min_entries muss 3 sein");
        assert_eq!(
            cfg.min_total_salience, 60,
            "min_total_salience muss 60 sein"
        );
        assert_eq!(cfg.max_lesson_chars, 400, "max_lesson_chars muss 400 sein");
    }
}
