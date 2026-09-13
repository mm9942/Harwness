//! Kontextauswahl — verbindet `ModelRuntimeProfile.context_policy` mit `RenderedContext`.
//!
//! # Spezifikation
//! Implementiert gemäß `coding-philosophy.md` §18 (Memory-independent canonical store;
//! model-aware rendering). Aufbauend auf `context_policy.rs`, verbindet aber
//! `ModelRuntimeProfile.context_policy` → `RenderedContext`. Bleibt
//! cross-crate-Dependency-frei (kein `harw-model-catalog`-Import).
//!
//! # Verantwortungsbereich
//! Dieses Modul kombiniert LTM (via `store::Memory`-Trait), STM
//! (`short_term::ShortTermMemory`) und epistemische Signals
//! (`epistemic::EpistemicSignal`) zu einem einzigen, rollen-gewichteten
//! `SelectionResult` pro Turn. Es delegiert das eigentliche Rendering an
//! `context_policy::render_turn_context` und führt selbst nur die
//! Signals-Filterung und das rollen-spezifische Capping durch.
//!
//! # Schlüsseltypen
//! - [`SelectionRole`] — Modell-Rolle für Selektions-Entscheidungen.
//! - [`SelectionRequest`] — Eingabe-Parameter für einen Turn-Rendering-Aufruf.
//! - [`SelectionResult`] — Kombiniertes Ergebnis aus `RenderedContext` + Signals.
//! - [`select_for_turn`] — Hauptfunktion.
//! - [`select_for_turn_no_signals`] — Convenience-Wrapper ohne Signals.
//!
//! # Nebenläufigkeit
//! `select_for_turn` ist zustandslos. Nebenläufigkeitsgarantien folgen aus
//! `M: Send + Sync` und `ShortTermMemory: Sync`. `EpistemicSignal` ist ein
//! reiner Wert-Typ (`Send + Sync`). Die Funktion spawnt keine Threads.
//!
//! # Fehler
//! Gibt [`crate::error::MemoryError`] weiter; eigene Varianten werden nicht
//! eingeführt.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_memory::context_selector::{SelectionRequest, SelectionRole, select_for_turn_no_signals};
//! use harw_memory::context_policy::ContextPolicy;
//! use harw_memory::file_store::FileMemoryStore;
//! use harw_memory::short_term::ShortTermMemory;
//! use time::OffsetDateTime;
//!
//! let store = FileMemoryStore::open("/tmp/harw-cs-example").unwrap();
//! let stm = ShortTermMemory::new("session-1", 32, 2048);
//! let req = SelectionRequest {
//!     policy: ContextPolicy::Balanced,
//!     role: SelectionRole::Orchestrator,
//!     namespace: None,
//!     keywords: &[],
//!     min_stm_salience: 0,
//!     min_promotion_score: None,
//! };
//! let result = select_for_turn_no_signals(&store, &stm, req, OffsetDateTime::now_utc()).unwrap();
//! assert!(result.selected_signals.is_empty());
//! ```

use serde::{Deserialize, Serialize};

// ── SelectionRole ─────────────────────────────────────────────────────────────

/// Modell-Rolle für Selektions-Entscheidungen.
///
/// # Beschreibung
/// Wird von der Runtime gesetzt, nicht vom Modell selbst — Authority-Reduktion
/// nach `philosophy.md` §12. Die Rolle steuert das rollen-spezifische Cap für
/// [`SelectionResult::selected_signals`]:
/// - `Orchestrator` / `RepositoryWorker`: bis zu 20 Signals.
/// - `FocusedCodingWorker` / `Verifier`: bis zu 8 Signals.
/// - `Scout`: bis zu 4 Signals.
///
/// # Serialisierung
/// `snake_case` — z. B. `"focused_coding_worker"`.
///
/// # Nebenläufigkeit
/// `Copy`-Typ, `Send + Sync`.
///
/// # Beispiel
/// ```rust
/// use harw_memory::context_selector::SelectionRole;
/// let json = serde_json::to_string(&SelectionRole::FocusedCodingWorker).unwrap();
/// assert_eq!(json, r#""focused_coding_worker""#);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionRole {
    /// Orchestrator-Modell — breites Signal-Fenster (bis zu 20).
    Orchestrator,
    /// Repository-Worker — breites Signal-Fenster (bis zu 20).
    RepositoryWorker,
    /// Fokussierter Coding-Worker — schmales Signal-Fenster (bis zu 8).
    FocusedCodingWorker,
    /// Verifier — schmales Signal-Fenster (bis zu 8).
    Verifier,
    /// Scout — sehr schmales Signal-Fenster (bis zu 4).
    Scout,
}

impl SelectionRole {
    /// Gibt die maximale Anzahl selektierbarer Epistemic-Signals für diese Rolle zurück.
    ///
    /// # Beschreibung
    /// Implementiert das rollen-spezifische Cap aus `coding-philosophy.md` §18:
    /// - `Orchestrator` / `RepositoryWorker` → 20.
    /// - `FocusedCodingWorker` / `Verifier` → 8.
    /// - `Scout` → 4.
    ///
    /// # Rückgabe
    /// `usize` — maximale Anzahl Signals.
    ///
    /// # Nebenläufigkeit
    /// Rein funktional, thread-safe.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_memory::context_selector::SelectionRole;
    /// assert_eq!(SelectionRole::Scout.signal_cap(), 4);
    /// assert_eq!(SelectionRole::Orchestrator.signal_cap(), 20);
    /// ```
    #[must_use]
    pub const fn signal_cap(self) -> usize {
        match self {
            Self::Orchestrator | Self::RepositoryWorker => 20,
            Self::FocusedCodingWorker | Self::Verifier => 8,
            Self::Scout => 4,
        }
    }
}

// ── SelectionRequest ──────────────────────────────────────────────────────────

/// Feingranulare Auswahl-Optionen für ein Turn-Rendering.
///
/// # Beschreibung
/// Bündelt alle Parameter, die `select_for_turn` für einen einzelnen Turn
/// benötigt: Policy, Rolle, Namespace-/Keyword-Filter und Schwellwerte.
///
/// # Felder
/// - `policy` ([`crate::context_policy::ContextPolicy`]): Policy für Token-Budgets.
/// - `role` ([`SelectionRole`]): Modell-Rolle — steuert Signal-Cap.
/// - `namespace` (`Option<&str>`): Optionaler Namespace-Filter für WARM-Recall.
/// - `keywords` (`&[&str]`): Keyword-Filter für WARM-Recall.
/// - `min_stm_salience` (`u8`): Salience-Schwelle für STM-Einträge (0..=100).
/// - `min_promotion_score` (`Option<i32>`): Minimaler `PromotionScore::total()`
///   für Signals; `None` = kein Filter.
///
/// # Nebenläufigkeit
/// Enthält nur Referenzen; lifetime `'a` bindet die Borrows. Kein interner Zustand.
///
/// # Beispiel
/// ```rust
/// use harw_memory::context_selector::{SelectionRequest, SelectionRole};
/// use harw_memory::context_policy::ContextPolicy;
///
/// let req = SelectionRequest {
///     policy: ContextPolicy::TightSelect,
///     role: SelectionRole::Scout,
///     namespace: Some("domain/rust"),
///     keywords: &["clippy"],
///     min_stm_salience: 40,
///     min_promotion_score: Some(50),
/// };
/// assert_eq!(req.role, SelectionRole::Scout);
/// ```
#[derive(Debug, Clone)]
pub struct SelectionRequest<'a> {
    /// Policy für Token-Budgets (lokal gespiegelt aus Model-Catalog).
    pub policy: crate::context_policy::ContextPolicy,
    /// Modell-Rolle — Authority-Reduktion nach philosophy.md §12.
    pub role: SelectionRole,
    /// Optionaler Namespace-Filter für WARM-Recall.
    pub namespace: Option<&'a str>,
    /// Keyword-Filter für WARM-Recall (alle müssen matchen).
    pub keywords: &'a [&'a str],
    /// Optionale Salience-Schwelle für STM-Einträge (0..=100).
    pub min_stm_salience: u8,
    /// Optionaler minimaler PromotionScore für Epistemic-Signals; `None` = kein Filter.
    pub min_promotion_score: Option<i32>,
}

// ── SelectionResult ───────────────────────────────────────────────────────────

/// Ergebnis der Selektion — kombiniert `RenderedContext` mit epistemisch
/// gefilterten Signals.
///
/// # Beschreibung
/// `base` enthält den vollständig gerenderten Turn-Kontext (HOT + STM + WARM).
/// `selected_signals` enthält die nach Rolle gecappten, nach Score absteigend
/// sortierten epistemischen Signals. `scanned_signals` gibt an, wie viele
/// Signals der Filter insgesamt gesehen hat (vor Capping).
///
/// # Felder
/// - `base` ([`crate::context_policy::RenderedContext`]): Gerenderter Turn-Kontext.
/// - `selected_signals` (`Vec<EpistemicSignal>`): Selektierte hoch-relevante Signals.
/// - `scanned_signals` (`usize`): Anzahl der vom Filter gesehenen Signals (vor Cap).
///
/// # Nebenläufigkeit
/// Reiner Wert-Typ, `Send + Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::context_selector::{SelectionRequest, SelectionRole, select_for_turn_no_signals};
/// use harw_memory::context_policy::ContextPolicy;
/// use harw_memory::file_store::FileMemoryStore;
/// use harw_memory::short_term::ShortTermMemory;
/// use time::OffsetDateTime;
///
/// let store = FileMemoryStore::open("/tmp/harw-sr-doc").unwrap();
/// let stm = ShortTermMemory::new("s", 8, 512);
/// let req = SelectionRequest {
///     policy: ContextPolicy::Balanced,
///     role: SelectionRole::Verifier,
///     namespace: None,
///     keywords: &[],
///     min_stm_salience: 0,
///     min_promotion_score: None,
/// };
/// let res = select_for_turn_no_signals(&store, &stm, req, OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(res.scanned_signals, 0);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResult {
    /// Vollständig gerenderter Turn-Kontext (HOT + STM + WARM).
    pub base: crate::context_policy::RenderedContext,
    /// Selektierte hoch-relevante Epistemic-Signals (absteigend nach Score sortiert).
    pub selected_signals: Vec<crate::epistemic::EpistemicSignal>,
    /// Anzahl der Signals, die der Filter insgesamt gesehen hat (vor Capping).
    pub scanned_signals: usize,
}

// ── select_for_turn ───────────────────────────────────────────────────────────

/// Kombinierte Selektion aus LTM + STM + epistemischer Signals-Filterung.
///
/// # Beschreibung
/// Implementiert den Selektions-Algorithmus aus `coding-philosophy.md` §18.
/// Ablauf:
///
/// 1. STM auf `salience >= min_stm_salience` filtern und den gefilterten STM
///    mit dem Policy-Budget rendern; HOT und WARM via
///    `context_policy::render_turn_context` laden → `base`.
/// 2. `signals` nach `promotion_score().total() >= min_promotion_score` filtern
///    (nur wenn `min_promotion_score` gesetzt ist).
/// 3. `signals` nach `!is_expired(now)` und
///    `outcome != Some(OutcomeVerdict::Refuted)` filtern.
/// 4. Rollen-spezifisches Cap anwenden:
///    - `Orchestrator` / `RepositoryWorker`: bis zu 20.
///    - `FocusedCodingWorker` / `Verifier`: bis zu 8.
///    - `Scout`: bis zu 4.
/// 5. Absteigend nach `promotion_score().total()` sortieren.
///
/// # Argumente
/// - `store` (`&M`): LTM-Backend (implementiert [`crate::store::Memory`]).
/// - `stm` (`&ShortTermMemory`): In-Process Short-Term Memory.
/// - `signals` (`&[EpistemicSignal]`): Epistemische Signals (vom Aufrufer bereitgestellt).
/// - `request` ([`SelectionRequest`]): Alle Selektions-Parameter.
/// - `now` ([`time::OffsetDateTime`]): Referenzzeitpunkt für `is_expired`-Prüfung.
///
/// # Rückgabe
/// `Ok(SelectionResult)` mit gefülltem `base` und nach Score sortierten Signals.
///
/// # Fehler
/// - [`crate::error::MemoryError::Io`]: Lesefehler in HOT oder WARM (aus `render_turn_context`).
/// - [`crate::error::MemoryError::TierOverflow`]: HOT-Datei ist korrupt (aus Store).
///
/// # Nebenläufigkeit
/// Zustandslos; thread-sicher wenn `M: Send + Sync` und `ShortTermMemory: Sync`.
/// Keine Locks werden selbst erworben — Synchronisation liegt bei `store` und `stm`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::context_selector::{SelectionRequest, SelectionRole, select_for_turn};
/// use harw_memory::context_policy::ContextPolicy;
/// use harw_memory::file_store::FileMemoryStore;
/// use harw_memory::short_term::ShortTermMemory;
/// use time::OffsetDateTime;
///
/// let store = FileMemoryStore::open("/tmp/harw-sft-doc").unwrap();
/// let stm   = ShortTermMemory::new("s", 32, 2048);
/// let req   = SelectionRequest {
///     policy: ContextPolicy::Balanced,
///     role: SelectionRole::Orchestrator,
///     namespace: None,
///     keywords: &[],
///     min_stm_salience: 0,
///     min_promotion_score: None,
/// };
/// let res = select_for_turn(&store, &stm, &[], req, OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(res.scanned_signals, 0);
/// ```
pub fn select_for_turn<M: crate::store::Memory>(
    store: &M,
    stm: &crate::short_term::ShortTermMemory,
    signals: &[crate::epistemic::EpistemicSignal],
    request: SelectionRequest<'_>,
    now: time::OffsetDateTime,
) -> crate::error::MemoryResult<SelectionResult> {
    // Schritt 1: Basis-Rendering via context_policy. Für STM wird dieselbe
    // Auswahl- und Budgetlogik wie in `ShortTermMemory::render` verwendet,
    // aber erst nach dem Salience-Filter.
    let mut base = crate::context_policy::render_turn_context(
        store,
        stm,
        request.policy,
        request.namespace,
        request.keywords,
    )?;
    if request.min_stm_salience > 0 {
        base.stm = render_stm_above_salience(stm, request.policy, request.min_stm_salience);
        let hot_estimate = base.hot.len() / 4;
        let stm_estimate = base.stm.len() / 4;
        let warm_estimate: usize = base.warm.iter().map(|slice| slice.content.len() / 4).sum();
        base.token_estimate = hot_estimate + stm_estimate + warm_estimate;
    }

    let scanned_signals = signals.len();

    // Schritt 2–3: Signals filtern.
    let mut filtered: Vec<&crate::epistemic::EpistemicSignal> = signals
        .iter()
        .filter(|sig| {
            // Schritt 2: Minimaler PromotionScore (falls gesetzt).
            if let Some(min_score) = request.min_promotion_score {
                if sig.promotion_score().total() < min_score {
                    return false;
                }
            }
            // Schritt 3a: Kein abgelaufenes Signal.
            if sig.is_expired(now) {
                return false;
            }
            // Schritt 3b: Kein widerlegtes Signal.
            if sig.outcome == Some(crate::epistemic::OutcomeVerdict::Refuted) {
                return false;
            }
            true
        })
        .collect();

    // Schritt 5: Absteigend nach promotion_score().total() sortieren (vor Cap,
    // damit wir die besten Einträge behalten).
    filtered.sort_by(|a, b| {
        b.promotion_score()
            .total()
            .cmp(&a.promotion_score().total())
    });

    // Schritt 4: Rollen-spezifisches Cap.
    let cap = request.role.signal_cap();
    filtered.truncate(cap);

    let selected_signals: Vec<crate::epistemic::EpistemicSignal> =
        filtered.into_iter().cloned().collect();

    Ok(SelectionResult {
        base,
        selected_signals,
        scanned_signals,
    })
}

/// Rendert STM-Einträge oberhalb der angeforderten Salience-Schwelle.
///
/// Die Reihenfolge und das Token-Budget entsprechen exakt
/// [`crate::short_term::ShortTermMemory::render`]: jüngste Einträge zuerst,
/// stabile Sortierung bei gleichen Zeitstempeln und gierige Budgetauswahl.
fn render_stm_above_salience(
    stm: &crate::short_term::ShortTermMemory,
    policy: crate::context_policy::ContextPolicy,
    min_salience: u8,
) -> String {
    let budget = crate::context_policy::ContextBudget::for_policy(policy);
    let max_tokens = budget.max_total_tokens * (budget.stm_percent as usize) / 100;
    let mut sorted: Vec<crate::short_term::StmEntry> = stm
        .snapshot()
        .into_iter()
        .filter(|entry| entry.salience >= min_salience)
        .collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.at));

    let mut selected = Vec::new();
    let mut used_tokens = 0;
    for entry in &sorted {
        let tokens = (entry.content.len() / 4).max(1);
        if used_tokens + tokens <= max_tokens {
            selected.push(entry);
            used_tokens += tokens;
        }
    }

    let mut rendered = String::with_capacity(max_tokens * 4);
    for entry in selected {
        rendered.push('[');
        rendered.push_str(entry.role.as_kebab());
        rendered.push_str("] ");
        rendered.push_str(&entry.content);
        rendered.push('\n');
    }
    rendered
}

// ── select_for_turn_no_signals ────────────────────────────────────────────────

/// Convenience-Wrapper: leerer Signals-Slice.
///
/// # Beschreibung
/// Delegiert an [`select_for_turn`] mit `signals = &[]`. Nützlich, wenn der
/// Aufrufer keine epistemischen Signals hat oder diese Schicht überspringen
/// möchte. Das Ergebnis ist identisch mit einem Aufruf von `select_for_turn`
/// mit leerem Slice: `scanned_signals == 0`, `selected_signals` leer.
///
/// # Argumente
/// - `store` (`&M`): LTM-Backend.
/// - `stm` (`&ShortTermMemory`): In-Process STM.
/// - `request` ([`SelectionRequest`]): Selektions-Parameter.
/// - `now` ([`time::OffsetDateTime`]): Referenzzeitpunkt.
///
/// # Rückgabe
/// `Ok(SelectionResult)` — identisch zu `select_for_turn(..., &[], ...)`.
///
/// # Fehler
/// Wie [`select_for_turn`].
///
/// # Nebenläufigkeit
/// Zustandslos; thread-sicher.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::context_selector::{SelectionRequest, SelectionRole, select_for_turn_no_signals};
/// use harw_memory::context_policy::ContextPolicy;
/// use harw_memory::file_store::FileMemoryStore;
/// use harw_memory::short_term::ShortTermMemory;
/// use time::OffsetDateTime;
///
/// let store = FileMemoryStore::open("/tmp/harw-no-sig-doc").unwrap();
/// let stm   = ShortTermMemory::new("s", 32, 2048);
/// let req   = SelectionRequest {
///     policy: ContextPolicy::BroadContext,
///     role: SelectionRole::RepositoryWorker,
///     namespace: None,
///     keywords: &[],
///     min_stm_salience: 0,
///     min_promotion_score: None,
/// };
/// let res = select_for_turn_no_signals(&store, &stm, req, OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(res.scanned_signals, 0);
/// assert!(res.selected_signals.is_empty());
/// ```
pub fn select_for_turn_no_signals<M: crate::store::Memory>(
    store: &M,
    stm: &crate::short_term::ShortTermMemory,
    request: SelectionRequest<'_>,
    now: time::OffsetDateTime,
) -> crate::error::MemoryResult<SelectionResult> {
    select_for_turn(store, stm, &[], request, now)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_policy::ContextPolicy;
    use crate::epistemic::{
        Confidence, EpistemicSignal, MemoryScope, OutcomeVerdict, Provenance, Validity,
    };
    use crate::file_store::FileMemoryStore;
    use crate::short_term::{ShortTermMemory, StmRole};
    use time::{Duration, OffsetDateTime};

    // ── Hilfsfunktionen ───────────────────────────────────────────────────────

    /// Erzeugt ein eindeutiges temporäres Verzeichnis.
    fn tmp_root(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("harw-cs-{}-{}-{}", tag, std::process::id(), id))
    }

    /// Öffnet einen frischen `FileMemoryStore` in einem neuen Temp-Verzeichnis.
    fn open_store(tag: &str) -> (FileMemoryStore, std::path::PathBuf) {
        let root = tmp_root(tag);
        let _ = std::fs::remove_dir_all(&root);
        let store = FileMemoryStore::open(&root).expect("open store");
        (store, root)
    }

    /// Erzeugt ein minimales, gültiges `EpistemicSignal`.
    fn make_signal(id: &str, salience: u8, confirmations: u32) -> EpistemicSignal {
        EpistemicSignal {
            id: id.to_owned(),
            statement: format!("Signal {id}"),
            origin: Provenance::AgentObservation {
                task_id: "task-1".to_owned(),
            },
            confidence: Confidence::High,
            validity: Validity::Permanent,
            scope: MemoryScope::UserGlobal,
            contradictions: vec![],
            outcome: None,
            created_at: OffsetDateTime::now_utc(),
            last_used: None,
            independent_confirmations: confirmations,
            salience,
        }
    }

    /// Erzeugt einen `SelectionRequest` mit Standardwerten.
    fn default_request(role: SelectionRole) -> SelectionRequest<'static> {
        SelectionRequest {
            policy: ContextPolicy::Balanced,
            role,
            namespace: None,
            keywords: &[],
            min_stm_salience: 0,
            min_promotion_score: None,
        }
    }

    // ── Test 1: Serde snake_case für SelectionRole ────────────────────────────

    /// Prüft, dass `FocusedCodingWorker` als `"focused_coding_worker"` serialisiert
    /// und zurück deserialisiert werden kann.
    ///
    /// Spezifikation: `#[serde(rename_all = "snake_case")]` auf `SelectionRole`.
    #[test]
    fn role_serde_snake_case() {
        let json = serde_json::to_string(&SelectionRole::FocusedCodingWorker).expect("serialize");
        assert_eq!(json, r#""focused_coding_worker""#);

        let back: SelectionRole = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, SelectionRole::FocusedCodingWorker);
    }

    // ── Test 2: Leere Eingabe → leere Selektion ───────────────────────────────

    /// Prüft, dass leerer Store + leere STM + leere Signals zu einem Ergebnis
    /// mit `scanned_signals == 0` und `selected_signals` leer führen.
    ///
    /// Spezifikation: Schritt 2–5 von `select_for_turn`.
    #[test]
    fn empty_input_produces_empty_selection() {
        let (store, root) = open_store("empty");
        let stm = ShortTermMemory::new("s-empty", 32, 2048);
        let now = OffsetDateTime::now_utc();

        let result = select_for_turn(
            &store,
            &stm,
            &[],
            default_request(SelectionRole::Orchestrator),
            now,
        )
        .expect("select");

        assert_eq!(result.scanned_signals, 0, "scanned_signals muss 0 sein");
        assert!(
            result.selected_signals.is_empty(),
            "selected_signals muss leer sein"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 3: Orchestrator-Cap bei 20 ──────────────────────────────────────

    /// Prüft, dass bei Rolle `Orchestrator` maximal 20 Signals selektiert werden,
    /// auch wenn 30 Signals mit hohem PromotionScore vorliegen.
    ///
    /// Spezifikation: Schritt 4, Orchestrator → cap 20.
    #[test]
    fn role_orchestrator_caps_at_20() {
        let (store, root) = open_store("orch-cap");
        let stm = ShortTermMemory::new("s-orch", 32, 2048);
        let now = OffsetDateTime::now_utc();

        // 30 Signals mit hohem Score.
        let signals: Vec<EpistemicSignal> = (0..30)
            .map(|i| make_signal(&format!("sig-{i}"), 90, 5))
            .collect();

        let result = select_for_turn(
            &store,
            &stm,
            &signals,
            default_request(SelectionRole::Orchestrator),
            now,
        )
        .expect("select");

        assert!(
            result.selected_signals.len() <= 20,
            "Orchestrator-Cap 20 verletzt: got {}",
            result.selected_signals.len()
        );
        assert_eq!(result.scanned_signals, 30, "scanned_signals muss 30 sein");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 4: Scout-Cap bei 4 ───────────────────────────────────────────────

    /// Prüft, dass bei Rolle `Scout` maximal 4 Signals selektiert werden,
    /// auch wenn 10 gültige Signals vorliegen.
    ///
    /// Spezifikation: Schritt 4, Scout → cap 4.
    #[test]
    fn role_scout_caps_at_4() {
        let (store, root) = open_store("scout-cap");
        let stm = ShortTermMemory::new("s-scout", 32, 2048);
        let now = OffsetDateTime::now_utc();

        let signals: Vec<EpistemicSignal> = (0..10)
            .map(|i| make_signal(&format!("sig-{i}"), 70, 2))
            .collect();

        let result = select_for_turn(
            &store,
            &stm,
            &signals,
            default_request(SelectionRole::Scout),
            now,
        )
        .expect("select");

        assert!(
            result.selected_signals.len() <= 4,
            "Scout-Cap 4 verletzt: got {}",
            result.selected_signals.len()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 5: min_promotion_score filtert niedrige Scores aus ───────────────

    /// Prüft, dass Signals mit `promotion_score().total() < min_promotion_score`
    /// nicht in `selected_signals` erscheinen.
    ///
    /// Spezifikation: Schritt 2.
    #[test]
    fn min_promotion_score_filters() {
        let (store, root) = open_store("min-score");
        let stm = ShortTermMemory::new("s-score", 32, 2048);
        let now = OffsetDateTime::now_utc();

        // Signal mit salience=10, confirmations=0 → total ≈ 10.
        let low = make_signal("low", 10, 0);
        // Signal mit salience=80, confirmations=3 → total ≈ 80 + 30 = 110.
        let high = make_signal("high", 80, 3);
        let signals = vec![low, high];

        let mut req = default_request(SelectionRole::Orchestrator);
        req.min_promotion_score = Some(50); // schließt "low" aus.

        let result = select_for_turn(&store, &stm, &signals, req, now).expect("select");

        assert_eq!(result.scanned_signals, 2);
        assert_eq!(
            result.selected_signals.len(),
            1,
            "Nur 'high' darf selektiert werden"
        );
        assert_eq!(
            result.selected_signals[0].id, "high",
            "Selektiertes Signal muss 'high' sein"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 6: Refuted Signals werden ausgeschlossen ─────────────────────────

    /// Prüft, dass Signals mit `outcome = Some(Refuted)` nicht in
    /// `selected_signals` erscheinen.
    ///
    /// Spezifikation: Schritt 3b.
    #[test]
    fn refuted_signals_excluded() {
        let (store, root) = open_store("refuted");
        let stm = ShortTermMemory::new("s-refuted", 32, 2048);
        let now = OffsetDateTime::now_utc();

        let mut refuted = make_signal("refuted-sig", 90, 5);
        refuted.outcome = Some(OutcomeVerdict::Refuted);

        let valid = make_signal("valid-sig", 80, 3);
        let signals = vec![refuted, valid];

        let result = select_for_turn(
            &store,
            &stm,
            &signals,
            default_request(SelectionRole::Orchestrator),
            now,
        )
        .expect("select");

        assert_eq!(result.scanned_signals, 2);
        assert!(
            result
                .selected_signals
                .iter()
                .all(|s| s.id != "refuted-sig"),
            "Refuted-Signal darf nicht selektiert werden"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 7: Expired Signals werden ausgeschlossen ─────────────────────────

    /// Prüft, dass Signals mit `Validity::Expired` oder abgelaufenem
    /// `ExpiresAt` nicht in `selected_signals` erscheinen.
    ///
    /// Spezifikation: Schritt 3a.
    #[test]
    fn expired_signals_excluded() {
        let (store, root) = open_store("expired");
        let stm = ShortTermMemory::new("s-expired", 32, 2048);
        let now = OffsetDateTime::now_utc();

        // Signal mit Validity::Expired.
        let mut expired_variant = make_signal("expired-variant", 90, 5);
        expired_variant.validity = Validity::Expired;

        // Signal mit ExpiresAt in der Vergangenheit.
        let past = now - Duration::hours(2);
        let mut expired_time = make_signal("expired-time", 85, 4);
        expired_time.validity = Validity::ExpiresAt { at: past };

        // Gültiges Signal.
        let valid = make_signal("valid", 70, 2);

        let signals = vec![expired_variant, expired_time, valid];

        let result = select_for_turn(
            &store,
            &stm,
            &signals,
            default_request(SelectionRole::Orchestrator),
            now,
        )
        .expect("select");

        assert_eq!(result.scanned_signals, 3);
        assert!(
            result
                .selected_signals
                .iter()
                .all(|s| s.id != "expired-variant" && s.id != "expired-time"),
            "Abgelaufene Signals dürfen nicht selektiert werden; got: {:?}",
            result
                .selected_signals
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            result.selected_signals.len(),
            1,
            "Nur 'valid' darf selektiert werden"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 8: Signals absteigend nach Score sortiert ────────────────────────

    /// Prüft, dass `selected_signals` absteigend nach `promotion_score().total()`
    /// sortiert ist — höchster Score zuerst.
    ///
    /// Spezifikation: Schritt 5.
    #[test]
    fn selected_signals_sorted_by_score_desc() {
        let (store, root) = open_store("sorted");
        let stm = ShortTermMemory::new("s-sorted", 32, 2048);
        let now = OffsetDateTime::now_utc();

        // Drei Signals mit unterschiedlichen Scores (via salience).
        // salience=30 → total ≈ 30; salience=80 → total ≈ 80; salience=50 → total ≈ 50.
        let low = make_signal("low", 30, 0);
        let high = make_signal("high", 80, 0);
        let mid = make_signal("mid", 50, 0);
        // Absichtlich in nicht-sortierter Reihenfolge übergeben.
        let signals = vec![low, high, mid];

        let result = select_for_turn(
            &store,
            &stm,
            &signals,
            default_request(SelectionRole::Orchestrator),
            now,
        )
        .expect("select");

        assert_eq!(result.selected_signals.len(), 3);

        // Prüfe absteigend.
        let scores: Vec<i32> = result
            .selected_signals
            .iter()
            .map(|s| s.promotion_score().total())
            .collect();

        for window in scores.windows(2) {
            assert!(
                window[0] >= window[1],
                "Signals müssen absteigend sortiert sein; Scores: {scores:?}"
            );
        }
        assert_eq!(
            result.selected_signals[0].id, "high",
            "Erstes Signal muss 'high' sein"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 9: Convenience-Funktion delegiert korrekt ────────────────────────

    /// Prüft, dass `select_for_turn_no_signals` das gleiche Ergebnis liefert
    /// wie `select_for_turn` mit leerem Slice.
    ///
    /// Spezifikation: `select_for_turn_no_signals`.
    #[test]
    fn select_for_turn_no_signals_delegates() {
        let (store, root) = open_store("no-signals");
        let stm = ShortTermMemory::new("s-nosig", 32, 2048);
        stm.push(StmRole::User, 80, "Testnachricht");
        let now = OffsetDateTime::now_utc();

        let req_a = SelectionRequest {
            policy: ContextPolicy::Balanced,
            role: SelectionRole::Verifier,
            namespace: None,
            keywords: &[],
            min_stm_salience: 0,
            min_promotion_score: None,
        };
        let req_b = SelectionRequest {
            policy: ContextPolicy::Balanced,
            role: SelectionRole::Verifier,
            namespace: None,
            keywords: &[],
            min_stm_salience: 0,
            min_promotion_score: None,
        };

        let res_direct = select_for_turn(&store, &stm, &[], req_a, now).expect("select direct");
        let res_convenience =
            select_for_turn_no_signals(&store, &stm, req_b, now).expect("select no-signals");

        assert_eq!(res_direct.scanned_signals, res_convenience.scanned_signals);
        assert_eq!(
            res_direct.selected_signals.len(),
            res_convenience.selected_signals.len()
        );
        assert_eq!(res_direct.base.policy, res_convenience.base.policy);
        assert_eq!(
            res_direct.base.token_estimate,
            res_convenience.base.token_estimate
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Test 10: STM-Salience-Schwelle wird vor dem Rendering angewandt ──────

    /// Prüft, dass STM-Einträge unterhalb der Schwelle weder den gerenderten
    /// Kontext noch dessen Budgetauswahl beeinflussen.
    #[test]
    fn min_stm_salience_filters_before_rendering() {
        let (store, root) = open_store("min-stm-salience");
        let stm = ShortTermMemory::new("s-min-stm-salience", 32, 2048);
        stm.push(StmRole::User, 80, "high-salience-older");
        stm.push(StmRole::Tool, 10, "low-salience");
        stm.push(StmRole::Assistant, 90, "high-salience-newer");

        let mut request = default_request(SelectionRole::Verifier);
        request.min_stm_salience = 50;

        let result =
            select_for_turn(&store, &stm, &[], request, OffsetDateTime::now_utc()).expect("select");

        assert_eq!(
            result.base.stm,
            "[assistant] high-salience-newer\n[user] high-salience-older\n"
        );
        assert!(!result.base.stm.contains("low-salience"));
        assert_eq!(result.base.token_estimate, result.base.stm.len() / 4);
        let _ = std::fs::remove_dir_all(&root);
    }
}
