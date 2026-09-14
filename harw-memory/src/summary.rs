//! Rollout-Summary-Extraktion und `/memory stats`-Aggregation.
//!
//! # Verantwortungsbereich
//! Zwei unabhängige Verantwortungsbereiche in einem Modul:
//!
//! 1. Transformiert [`crate::short_term::StmEntry`]-Sequenzen in kompakte
//!    [`crate::types::Signal::Reflection`]-Einträge, die in die
//!    Memory-Promotion-Pipeline eingespeist werden ([`RolloutSummary`],
//!    [`extract_summary`], [`extract_and_record`]). Reine Datentransformation
//!    — kein I/O außer dem abschließenden `store.record`-Aufruf.
//! 2. Aggregiert die Zahlen für den Befehl `/memory stats` aus
//!    `docs/design/memory-v3-ltm.md` §6 ([`MemoryStatsSummary`],
//!    [`summarize_memory_stats`], [`count_incoming_candidates`]).
//!
//! Folgt `docs/design/memory-v2.md` §3 (STM) und §5 (Signals),
//! `docs/design/memory-v3-ltm.md` §5.4 (Verfall) und §6 (Bedienung) sowie
//! `philosophy.md` §4 („Memory-Konsolidierung ist ein langlebiger Workflow") und
//! §16 Invariante 8 („Memory ist Promotion-Pipeline").
//!
//! # Schlüsseltypen
//! - [`RolloutSummary`] — Zusammenfassung einer abgeschlossenen Turn-Sequenz.
//! - [`SummaryConfig`] — Schwellenwerte für Mindest-Einträge, Mindest-Salience und Lesson-Länge.
//! - [`extract_summary`] — reine, infallible Datentransformation.
//! - [`extract_and_record`] — Extraktion + Persistenz als `Signal::Reflection`.
//! - [`MemoryStatsSummary`] — Zahlen für `/memory stats` (Design §6), `Display`-fähig
//!   für die deutsche Textausgabe.
//! - [`summarize_memory_stats`] — reine Aggregationsfunktion, die
//!   [`MemoryStatsSummary`] aus bereits geladenen Daten baut (kein I/O).
//! - [`count_incoming_candidates`] — kleiner I/O-Helfer, zählt `.md`-Dateien
//!   unter `facts/_incoming/` (Design §5.2/§6).
//!
//! # Nebenläufigkeit
//! `extract_summary` und `summarize_memory_stats` sind rein funktional und
//! threadunsicher nur durch Mut-Borrow auf internen Sortiertabellen — nach
//! außen hin unproblematisch. `extract_and_record` und
//! `count_incoming_candidates` delegieren I/O an [`crate::store::Memory`]
//! bzw. das Dateisystem.
//!
//! # Fehler
//! `extract_summary` und `summarize_memory_stats` sind infallibel.
//! `extract_and_record` propagiert [`crate::error::MemoryError`] aus
//! `store.record()`; `count_incoming_candidates` propagiert
//! [`crate::error::MemoryError::Io`].
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

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{MemoryError, MemoryResult};
use crate::facts::{Fact, FactScope, FactType};
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

// ── `/memory stats` (memory-v3-ltm.md §6) ───────────────────────────────────

/// Zahlen für den Befehl `/memory stats` aus Design §6.
///
/// # Beschreibung
/// Reiner Datencontainer, erzeugt von [`summarize_memory_stats`]. Implementiert
/// [`fmt::Display`] als die deutsche Textform, die `/memory stats` direkt
/// ausgeben kann.
///
/// # Felder
/// - `total_facts`: Anzahl aller geladenen Fakten (beide Scopes zusammen).
/// - `by_scope`: Anzahl je [`FactScope`], stabil in Reihenfolge `Project, Global`.
/// - `by_type`: Anzahl je [`FactType`], in [`FactType::ALL`]-Reihenfolge.
/// - `top_used`: die bis zu fünf meistgenutzten Fakten (`usage_count > 0`),
///   absteigend nach Zähler, bei Gleichstand aufsteigend nach Name.
/// - `incoming_candidates`: Anzahl Kandidaten unter `facts/_incoming/`.
/// - `last_maintenance`: Zeitpunkt des letzten Wartungslaufs, `None` wenn noch
///   keiner gelaufen ist.
/// - `facts_decayed`: Anzahl der Fakten, die beim letzten Wartungslauf laut
///   [`crate::heartbeat::FactDecayReport`] verfallen sind.
///
/// # Beispiel
/// ```rust
/// use harw_memory::summary::MemoryStatsSummary;
///
/// let s = MemoryStatsSummary {
///     total_facts: 0,
///     by_scope: Vec::new(),
///     by_type: Vec::new(),
///     top_used: Vec::new(),
///     incoming_candidates: 0,
///     last_maintenance: None,
///     facts_decayed: 0,
/// };
/// assert_eq!(s.total_facts, 0);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryStatsSummary {
    /// Anzahl aller geladenen Fakten (beide Scopes zusammen).
    pub total_facts: usize,
    /// Anzahl je [`FactScope`], stabil in Reihenfolge `Project, Global`.
    pub by_scope: Vec<(FactScope, usize)>,
    /// Anzahl je [`FactType`], in [`FactType::ALL`]-Reihenfolge.
    pub by_type: Vec<(FactType, usize)>,
    /// Bis zu fünf meistgenutzte Fakten (Name, Zähler), absteigend sortiert.
    pub top_used: Vec<(String, u64)>,
    /// Anzahl Kandidaten unter `facts/_incoming/`.
    pub incoming_candidates: usize,
    /// Zeitpunkt des letzten Wartungslaufs, `None` wenn noch keiner gelaufen ist.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_maintenance: Option<OffsetDateTime>,
    /// Anzahl beim letzten Wartungslauf verfallener Fakten.
    pub facts_decayed: usize,
}

/// Formatiert `ts` als `yyyy-mm-dd HH:MM` UTC; fällt auf den Unix-Zeitstempel
/// zurück, falls die Formatierung scheitert (kein Panic in `Display`).
fn format_maintenance_ts(ts: OffsetDateTime) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute] UTC");
    ts.format(&format)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}

impl fmt::Display for MemoryStatsSummary {
    /// Deutsche Textform für `/memory stats` (Design §6: „Nutzung, Verfall,
    /// Kandidaten").
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Gedächtnis-Statistik")?;
        writeln!(f, "Fakten gesamt: {}", self.total_facts)?;
        writeln!(f, "Nach Scope:")?;
        for (scope, count) in &self.by_scope {
            writeln!(f, "  {}: {count}", scope.as_str())?;
        }
        writeln!(f, "Nach Typ:")?;
        for (fact_type, count) in &self.by_type {
            writeln!(f, "  {}: {count}", fact_type.as_str())?;
        }
        if self.top_used.is_empty() {
            writeln!(f, "Meistgenutzte Fakten: keine")?;
        } else {
            writeln!(f, "Meistgenutzte Fakten:")?;
            for (name, count) in &self.top_used {
                writeln!(f, "  {name}: {count}×")?;
            }
        }
        writeln!(
            f,
            "Kandidaten in facts/_incoming/: {}",
            self.incoming_candidates
        )?;
        match self.last_maintenance {
            Some(ts) => writeln!(f, "Letzter Wartungslauf: {}", format_maintenance_ts(ts))?,
            None => writeln!(f, "Letzter Wartungslauf: noch nicht gelaufen")?,
        }
        write!(f, "Verfallene Fakten: {}", self.facts_decayed)
    }
}

/// Baut eine [`MemoryStatsSummary`] aus bereits geladenen Daten — reine
/// Funktion, kein I/O (Design §6).
///
/// # Beschreibung
/// Der Aufrufer (typischerweise der `/memory stats`-Kommando-Handler) lädt
/// `facts` über [`crate::facts::FactStore::list`] (beide Scopes
/// zusammengeführt), `usage` über [`crate::facts::FactStore::usage`] je Fakt
/// oder eine äquivalente Zählerquelle, `incoming_candidates` über
/// [`count_incoming_candidates`] und `last_maintenance`/`facts_decayed` aus
/// dem zuletzt persistierten Wartungslauf-Zustand
/// ([`crate::types::MaintenanceReport`]).
///
/// # Argumente
/// - `facts` (`&[Fact]`): alle geladenen Fakten, projektübergreifend zusammengeführt.
/// - `usage` (`&HashMap<String, u64>`): Nutzungszähler je Faktname; fehlende
///   Einträge zählen als `0`.
/// - `incoming_candidates` (`usize`): Anzahl Kandidaten unter `facts/_incoming/`.
/// - `last_maintenance` (`Option<OffsetDateTime>`): Zeitpunkt des letzten
///   Wartungslaufs.
/// - `facts_decayed` (`usize`): Anzahl beim letzten Wartungslauf verfallener Fakten.
///
/// # Rückgabe
/// Eine vollständig befüllte [`MemoryStatsSummary`].
///
/// # Panics
/// Keine.
///
/// # Beispiel
/// ```rust
/// use harw_memory::summary::summarize_memory_stats;
/// use std::collections::HashMap;
///
/// let summary = summarize_memory_stats(&[], &HashMap::new(), 0, None, 0);
/// assert_eq!(summary.total_facts, 0);
/// assert_eq!(summary.by_scope.len(), 2);
/// ```
#[must_use]
pub fn summarize_memory_stats(
    facts: &[Fact],
    usage: &HashMap<String, u64>,
    incoming_candidates: usize,
    last_maintenance: Option<OffsetDateTime>,
    facts_decayed: usize,
) -> MemoryStatsSummary {
    let mut by_scope_counts: HashMap<FactScope, usize> = HashMap::new();
    let mut by_type_counts: HashMap<FactType, usize> = HashMap::new();
    for fact in facts {
        *by_scope_counts.entry(fact.scope).or_insert(0) += 1;
        *by_type_counts.entry(fact.fact_type).or_insert(0) += 1;
    }

    let by_scope = [FactScope::Project, FactScope::Global]
        .into_iter()
        .map(|scope| (scope, *by_scope_counts.get(&scope).unwrap_or(&0)))
        .collect();

    let by_type = FactType::ALL
        .into_iter()
        .map(|fact_type| (fact_type, *by_type_counts.get(&fact_type).unwrap_or(&0)))
        .collect();

    let mut top_used: Vec<(String, u64)> = facts
        .iter()
        .filter_map(|fact| {
            let count = *usage.get(&fact.name).unwrap_or(&0);
            (count > 0).then(|| (fact.name.clone(), count))
        })
        .collect();
    top_used.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top_used.truncate(5);

    MemoryStatsSummary {
        total_facts: facts.len(),
        by_scope,
        by_type,
        top_used,
        incoming_candidates,
        last_maintenance,
        facts_decayed,
    }
}

/// Zählt Kandidaten-Dateien (`*.md`) unter `<root>/facts/_incoming/`
/// (Design §5.2 „Kandidat", §6 „Kandidaten").
///
/// # Beschreibung
/// Ein fehlendes `_incoming/`-Verzeichnis (noch keine Extraktion gelaufen)
/// liefert `Ok(0)` statt eines Fehlers — das ist der Normalzustand vor der
/// ersten Phase-1-Extraktion (Design §5.2).
///
/// # Argumente
/// - `root` (`impl AsRef<Path>`): Wurzel eines [`crate::facts::FactStore`]
///   (Projekt oder Global).
///
/// # Rückgabe
/// Anzahl der `.md`-Dateien direkt unter `<root>/facts/_incoming/`.
///
/// # Fehler
/// [`MemoryError::Io`], wenn das Verzeichnis existiert, aber nicht gelesen
/// werden kann (z. B. Rechteproblem).
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::summary::count_incoming_candidates;
///
/// let n = count_incoming_candidates("/tmp/harw-memory-example").unwrap();
/// assert_eq!(n, 0);
/// ```
pub fn count_incoming_candidates(root: impl AsRef<Path>) -> MemoryResult<usize> {
    let dir = root.as_ref().join("facts").join("_incoming");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(MemoryError::Io {
                path: dir,
                source: e,
            });
        }
    };
    let mut count = 0usize;
    for entry in entries {
        let entry = entry.map_err(|e| MemoryError::Io {
            path: dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            count += 1;
        }
    }
    Ok(count)
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

    // ── `/memory stats` (memory-v3-ltm.md §6) ─────────────────────────────────

    fn stats_fact(name: &str, scope: FactScope, fact_type: FactType) -> Fact {
        let now = OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: "Teststatistik-Fakt".to_owned(),
            fact_type,
            scope,
            created: now,
            updated: now,
            confidence: 0.8,
            sources: Vec::new(),
            tags: Vec::new(),
            body: "Inhalt.\n".to_owned(),
        }
    }

    // ── Test 9 ───────────────────────────────────────────────────────────────

    /// Zählt Fakten korrekt je Scope und Typ — auch Scopes/Typen ohne
    /// Vorkommen erscheinen mit Zähler `0` (deterministische Reihenfolge).
    #[test]
    fn summarize_memory_stats_counts_by_scope_and_type() {
        let facts = vec![
            stats_fact("a", FactScope::Project, FactType::Decision),
            stats_fact("b", FactScope::Project, FactType::Decision),
            stats_fact("c", FactScope::Project, FactType::Preference),
            stats_fact("d", FactScope::Global, FactType::Fact),
        ];
        let summary = summarize_memory_stats(&facts, &HashMap::new(), 0, None, 0);

        assert_eq!(summary.total_facts, 4);
        assert_eq!(
            summary.by_scope,
            vec![(FactScope::Project, 3), (FactScope::Global, 1)],
            "by_scope muss in Reihenfolge Project, Global stehen"
        );
        let decision_count = summary
            .by_type
            .iter()
            .find(|(t, _)| *t == FactType::Decision)
            .map(|(_, c)| *c);
        assert_eq!(decision_count, Some(2));
        let pitfall_count = summary
            .by_type
            .iter()
            .find(|(t, _)| *t == FactType::Pitfall)
            .map(|(_, c)| *c);
        assert_eq!(
            pitfall_count,
            Some(0),
            "Typen ohne Vorkommen müssen mit 0 erscheinen"
        );
        assert_eq!(summary.by_type.len(), FactType::ALL.len());
    }

    // ── Test 10 ──────────────────────────────────────────────────────────────

    /// Meistgenutzte Fakten: absteigend nach Zähler, bei Gleichstand
    /// aufsteigend nach Name, höchstens fünf, Zähler `0` fällt heraus.
    #[test]
    fn summarize_memory_stats_ranks_top_used_facts() {
        let facts: Vec<Fact> = (0..7)
            .map(|i| stats_fact(&format!("fakt-{i}"), FactScope::Project, FactType::Fact))
            .collect();
        let mut usage = HashMap::new();
        usage.insert("fakt-0".to_owned(), 10u64);
        usage.insert("fakt-1".to_owned(), 30u64);
        usage.insert("fakt-2".to_owned(), 30u64);
        usage.insert("fakt-3".to_owned(), 5u64);
        usage.insert("fakt-4".to_owned(), 20u64);
        usage.insert("fakt-5".to_owned(), 1u64);
        // fakt-6 bleibt ungenutzt (kein Eintrag) → fällt heraus.

        let summary = summarize_memory_stats(&facts, &usage, 0, None, 0);

        assert_eq!(summary.top_used.len(), 5, "höchstens fünf Einträge");
        assert_eq!(
            summary.top_used,
            vec![
                ("fakt-1".to_owned(), 30),
                ("fakt-2".to_owned(), 30),
                ("fakt-4".to_owned(), 20),
                ("fakt-0".to_owned(), 10),
                ("fakt-3".to_owned(), 5),
            ],
            "absteigend nach Zähler, bei Gleichstand aufsteigend nach Name"
        );
        assert!(
            summary.top_used.iter().all(|(name, _)| name != "fakt-6"),
            "ungenutzte Fakten dürfen nicht erscheinen"
        );
    }

    // ── Test 11 ──────────────────────────────────────────────────────────────

    /// `incoming_candidates`, `last_maintenance` und `facts_decayed` werden
    /// unverändert durchgereicht (reine Aggregation, kein I/O).
    #[test]
    fn summarize_memory_stats_passes_through_scalar_fields() {
        let now = OffsetDateTime::now_utc();
        let summary = summarize_memory_stats(&[], &HashMap::new(), 3, Some(now), 7);
        assert_eq!(summary.incoming_candidates, 3);
        assert_eq!(summary.last_maintenance, Some(now));
        assert_eq!(summary.facts_decayed, 7);
        assert_eq!(summary.total_facts, 0);
        assert_eq!(summary.by_scope, vec![(FactScope::Project, 0), (FactScope::Global, 0)]);
    }

    // ── Test 12 ──────────────────────────────────────────────────────────────

    /// Die deutsche `Display`-Textform enthält alle Kernzahlen.
    #[test]
    fn memory_stats_summary_display_contains_all_sections() {
        let facts = vec![stats_fact("tui-fix", FactScope::Project, FactType::Decision)];
        let mut usage = HashMap::new();
        usage.insert("tui-fix".to_owned(), 4u64);
        let now = OffsetDateTime::now_utc();
        let summary = summarize_memory_stats(&facts, &usage, 2, Some(now), 1);

        let text = summary.to_string();
        assert!(text.contains("Fakten gesamt: 1"));
        assert!(text.contains("tui-fix: 4×"));
        assert!(text.contains("Kandidaten in facts/_incoming/: 2"));
        assert!(text.contains("Letzter Wartungslauf:"));
        assert!(text.contains("Verfallene Fakten: 1"));
    }

    /// Ohne Wartungslauf und ohne Nutzung meldet die Textform das explizit,
    /// statt leere Abschnitte zu zeigen.
    #[test]
    fn memory_stats_summary_display_handles_empty_state() {
        let summary = summarize_memory_stats(&[], &HashMap::new(), 0, None, 0);
        let text = summary.to_string();
        assert!(text.contains("Meistgenutzte Fakten: keine"));
        assert!(text.contains("Letzter Wartungslauf: noch nicht gelaufen"));
    }

    // ── Test 13 ──────────────────────────────────────────────────────────────

    /// `count_incoming_candidates` zählt nur `.md`-Dateien und liefert `0`
    /// statt eines Fehlers, wenn `_incoming/` noch nicht existiert.
    #[test]
    fn count_incoming_candidates_counts_md_files_and_defaults_to_zero() {
        let root = tmp_root("incoming");
        assert_eq!(
            count_incoming_candidates(&root).unwrap(),
            0,
            "fehlendes Verzeichnis muss Ok(0) liefern"
        );

        let incoming = root.join("facts").join("_incoming");
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::write(incoming.join("kandidat-a.md"), "---\n---\n").unwrap();
        std::fs::write(incoming.join("kandidat-b.md"), "---\n---\n").unwrap();
        std::fs::write(incoming.join("notiz.txt"), "kein Kandidat").unwrap();

        assert_eq!(
            count_incoming_candidates(&root).unwrap(),
            2,
            "nur .md-Dateien zaehlen als Kandidaten"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
