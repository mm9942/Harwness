//! Auto-Compact-Policy: wann wird die Session-Historie verdichtet?
//!
//! # Modell
//!
//! Der Harness hält die dauerhafte Historie vollständig; das Kontextfenster
//! des Modells ist aber begrenzt, und lange Sessions (viele Tool-Ergebnisse)
//! laufen sonst in `context_window_exceeded`. Diese Policy entscheidet
//! **deterministisch und ohne Modellaufruf**, ob vor dem nächsten Turn eine
//! Verdichtung fällig ist.
//!
//! Zwei Trigger, bewusst kombiniert:
//!
//! 1. **Task-Ende (primär):** Wenn eine Aufgabe abgeschlossen ist und die
//!    Nutzung bereits spürbar ist, wird verdichtet — die Zusammenfassung
//!    eines abgeschlossenen Blocks ist semantisch sauberer als ein Schnitt
//!    mitten im Gedankengang.
//! 2. **Sicherheitsdeckel (sekundär):** Unabhängig vom Task-Status wird
//!    verdichtet, sobald die Nutzung einen festen Anteil des
//!    Kontextfensters überschreitet. Das verhindert, dass ein einzelner
//!    großer Task (z. B. Exploration mit vielen `fs.read`-Ergebnissen) das
//!    Fenster sprengt, bevor er endet.
//!
//! # Zahlen
//!
//! Keine fixe Token-Zahl (z. B. „alle 200 000") wäre portierbar:
//! Kontextfenster variieren zwischen 128 k (ältere Modelle) und 1 M+
//! (aktuelle Frontier-Modelle). Stattdessen arbeitet die Policy **relativ**
//! zum konfigurierten Fenster:
//!
//! - **Compact-Schwelle:** 70 % des Fensters. Der Compact-Turn selbst kostet
//!   Tokens (Prompt + Summary-Ausgabe); 30 % Headroom reichen dafür plus
//!   einer anschließenden normalen Antwort sicher aus.
//! - **Task-Ende-Schwelle:** 30 % des Fensters. Unterhalb davon lohnt die
//!   Verdichtung nicht — der Overhead eines Compact-Turns (Latenz, Kosten)
//!   übersteigt den Gewinn.
//!
//! Zusätzlich gilt als absolute Obergrenze der Standard-Deckel
//! [`DEFAULT_ABSOLUTE_CEILING_TOKENS`] (500 000 Input-Tokens): effektive
//! Budget-Schwelle = `min(70 % Fenster, Deckel)`. Bei Modellen mit kleinem
//! Fenster (≤ ~714 k) bleibt die relative Schwelle maßgeblich; der Deckel
//! begrenzt zusätzlich die akkumulierte Input-Nutzung großer Sessions
//! (langlaufende Orchestrator-Sessions mit sehr großen Fenstern).
//!
//! # Vertrauen & Sicherheit
//!
//! Die Policy **entscheidet nur**; die eigentliche Verdichtung (Summary durch
//! das Modell, Ersetzen der Historie) ist Aufgabe des Aufrufers. Die Policy
//! mutiert keine Historie und ruft kein Modell. Die dauerhafte Historie
//! bleibt Eigentum der Session; ein Compact ist immer ein neuer Turn, dessen
//! Ergebnis der Aufrufer explizit zurückschreibt.
//!
//! # Concurrency
//!
//! Alle Typen sind `Send + Sync` und zustandslos (abgesehen von den
//! konfigurierten Schwellen). Die Policy enthält keine Locks und keinen
//! IO-Pfad.
//!
//! # Examples
//!
//! ```
//! use harw_core::auto_compact::{AutoCompactPolicy, CompactDecision};
//!
//! // 200k-Fenster: Compact bei 140k, Task-Ende-Compact ab 60k.
//! let policy = AutoCompactPolicy::for_context_window(200_000);
//! assert_eq!(policy.compact_threshold_tokens(), 140_000);
//! assert_eq!(policy.task_end_threshold_tokens(), 60_000);
//!
//! assert_eq!(
//!     policy.decide(50_000, false),
//!     CompactDecision::None,
//! );
//! assert_eq!(
//!     policy.decide(150_000, false),
//!     CompactDecision::BudgetExceeded,
//! );
//! assert_eq!(
//!     policy.decide(80_000, true),
//!     CompactDecision::TaskCompleted,
//! );
//! ```

use serde::{Deserialize, Serialize};

/// Anteil des Fensters, ab dem ein Compact unabhängig vom Task-Status fällig
/// ist (70 %).
const COMPACT_THRESHOLD_NUM: u64 = 7;
const COMPACT_THRESHOLD_DEN: u64 = 10;

/// Anteil des Fensters, ab dem ein Compact am Task-Ende lohnt (30 %).
const TASK_END_THRESHOLD_NUM: u64 = 3;
const TASK_END_THRESHOLD_DEN: u64 = 10;

/// Ausgangs der Trigger-Prüfung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactDecision {
    /// Kein Compact fällig.
    None,
    /// Sicherheitsdeckel überschritten — sofort verdichten, unabhängig vom
    /// Task-Status. Dieser Trigger hat Vorrang vor [`Self::TaskCompleted`].
    BudgetExceeded,
    /// Task abgeschlossen und Nutzung über der Task-Ende-Schwelle —
    /// verdichten, bevor der nächste Task-Block beginnt.
    TaskCompleted,
    /// Harte Verdichtung am Beginn eines neuen Auftrags-Turns einer
    /// bestehenden Orchestrator-Session
    /// ([`AutoCompactPolicy::turn_start_target_tokens`]), ausgelöst von
    /// `turn_loop` vor der ersten Modellrunde — unabhängig von
    /// [`Self::BudgetExceeded`]/[`Self::TaskCompleted`].
    TurnStart,
}

impl CompactDecision {
    /// `true`, wenn ein Compact durchgeführt werden soll.
    #[must_use]
    pub fn should_compact(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Feste Obergrenze für die Budget-Schwelle unabhängig vom Kontextfenster
/// (Root-/Sub-Orchestrator-Sessions), siehe [`AutoCompactPolicy::with_absolute_ceiling`].
///
/// Auto-Compact löst bei 500 000 Input-Tokens aus (Nutzung aus
/// `last_round_usage.input_tokens` — der einzig verlässliche direkte Blick
/// auf die tatsächliche Input-Nutzung des Modells). Bei Modellen mit einem
/// Fenster ≤ ~714 k Tokens greift die relative 70 %-Schwelle zuerst; der
/// Deckel begrenzt zusätzlich die kumulierte Input-Nutzung langer Sessions.
pub const DEFAULT_ABSOLUTE_CEILING_TOKENS: u64 = 500_000;

/// Ziel-Token-Zahl für die harte Verdichtung am Beginn eines neuen Auftrags
/// bei Orchestrator-Sessions, siehe [`AutoCompactPolicy::with_turn_start_target`].
pub const DEFAULT_ORCHESTRATOR_TURN_START_TARGET_TOKENS: u64 = 24_000;

/// Deterministische Auto-Compact-Policy.
///
/// # Herleitung
///
/// Die Schwellen werden aus dem konfigurierten Kontextfenster abgeleitet
/// (`for_context_window`) und sind danach unveränderlich. Beide Schwellen
/// werden mit saturierender Arithmetik berechnet; ein Fenster von 0 ist
/// erlaubt und deaktiviert die Policy (alle Entscheidungen: `None`).
///
/// Optional lässt sich zusätzlich eine feste Obergrenze
/// (`with_absolute_ceiling`) und ein Ziel für harte Verdichtung am
/// Auftragsbeginn (`with_turn_start_target`) aufsetzen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoCompactPolicy {
    /// Konfiguriertes Kontextfenster in Tokens. 0 deaktiviert die Policy.
    context_window_tokens: u64,
    /// Ab dieser Nutzung: [`CompactDecision::BudgetExceeded`].
    compact_threshold_tokens: u64,
    /// Ab dieser Nutzung am Task-Ende: [`CompactDecision::TaskCompleted`].
    task_end_threshold_tokens: u64,
    /// Feste Obergrenze zusätzlich zur relativen Schwelle: effektive
    /// Budget-Schwelle = `min(compact_threshold_tokens, ceiling)`.
    #[serde(default)]
    absolute_ceiling_tokens: Option<u64>,
    /// Ziel-Token-Zahl für die harte Verdichtung am Turn-Start (nur von
    /// `turn_loop` ausgewertet, fließt nicht in `decide` ein).
    #[serde(default)]
    turn_start_target_tokens: Option<u64>,
}

impl AutoCompactPolicy {
    /// Leitet die Policy aus dem Kontextfenster ab.
    ///
    /// # Arguments
    /// - `context_window_tokens` (`u64`): effektives Fenster des aktiven
    ///   Modells in Tokens (z. B. 200 000 oder 1 000 000). 0 deaktiviert
    ///   die Policy.
    ///
    /// # Returns
    /// Eine Policy mit 70 %-Compact- und 30 %-Task-Ende-Schwelle.
    #[must_use]
    pub fn for_context_window(context_window_tokens: u64) -> Self {
        let compact_threshold_tokens =
            context_window_tokens.saturating_mul(COMPACT_THRESHOLD_NUM) / COMPACT_THRESHOLD_DEN;
        let task_end_threshold_tokens =
            context_window_tokens.saturating_mul(TASK_END_THRESHOLD_NUM) / TASK_END_THRESHOLD_DEN;
        Self {
            context_window_tokens,
            compact_threshold_tokens,
            task_end_threshold_tokens,
            absolute_ceiling_tokens: None,
            turn_start_target_tokens: None,
        }
    }

    /// Dieselbe Policy für ein anderes Kontextfenster (z. B. nach einem
    /// Modellwechsel): Schwellen neu abgeleitet, feste Obergrenze und
    /// Turn-Start-Ziel bleiben erhalten.
    #[must_use]
    pub fn rescaled(self, context_window_tokens: u64) -> Self {
        Self::for_context_window(context_window_tokens)
            .with_absolute_ceiling(self.absolute_ceiling_tokens)
            .with_turn_start_target(self.turn_start_target_tokens)
    }

    /// Deaktivierte Policy (Fenster 0) — entscheidet immer `None`.
    #[must_use]
    pub fn disabled() -> Self {
        Self::for_context_window(0)
    }

    /// Setzt eine feste Obergrenze zusätzlich zur relativen Compact-Schwelle.
    ///
    /// # Arguments
    /// - `max_tokens` (`Option<u64>`): harte Obergrenze in Tokens. `None`
    ///   entfernt die Obergrenze wieder. Die effektive Budget-Schwelle ist
    ///   danach `min(compact_threshold_tokens, max_tokens)`.
    ///
    /// # Returns
    /// Die angepasste Policy (Builder-Stil, konsumiert `self`).
    #[must_use]
    pub fn with_absolute_ceiling(mut self, max_tokens: Option<u64>) -> Self {
        self.absolute_ceiling_tokens = max_tokens;
        self
    }

    /// Setzt das Ziel für die harte Verdichtung am Beginn eines neuen
    /// Auftrags (Turn-Start einer bestehenden Session mit Verlauf).
    ///
    /// # Arguments
    /// - `target_tokens` (`Option<u64>`): Ziel-Token-Zahl, auf die
    ///   `turn_loop` die Historie zu Beginn eines neuen Auftrags-Turns
    ///   verdichtet, sofern die geschätzte Verlaufs-Token-Zahl darüber liegt.
    ///   `None` deaktiviert die Turn-Start-Verdichtung.
    ///
    /// # Returns
    /// Die angepasste Policy (Builder-Stil, konsumiert `self`).
    #[must_use]
    pub fn with_turn_start_target(mut self, target_tokens: Option<u64>) -> Self {
        self.turn_start_target_tokens = target_tokens;
        self
    }

    /// Das konfigurierte Turn-Start-Verdichtungsziel, falls gesetzt.
    #[must_use]
    pub fn turn_start_target_tokens(&self) -> Option<u64> {
        self.turn_start_target_tokens
    }

    /// Effektive Budget-Schwelle unter Berücksichtigung der optionalen
    /// festen Obergrenze: `min(compact_threshold_tokens, ceiling)`.
    fn effective_compact_threshold_tokens(&self) -> u64 {
        match self.absolute_ceiling_tokens {
            Some(ceiling) => self.compact_threshold_tokens.min(ceiling),
            None => self.compact_threshold_tokens,
        }
    }

    /// Das konfigurierte Kontextfenster.
    #[must_use]
    pub fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }

    /// Schwelle für [`CompactDecision::BudgetExceeded`].
    #[must_use]
    pub fn compact_threshold_tokens(&self) -> u64 {
        self.compact_threshold_tokens
    }

    /// Schwelle für [`CompactDecision::TaskCompleted`].
    #[must_use]
    pub fn task_end_threshold_tokens(&self) -> u64 {
        self.task_end_threshold_tokens
    }

    /// Entscheidet, ob vor dem nächsten Turn verdichtet werden soll.
    ///
    /// # Arguments
    /// - `tokens_used` (`u64`): bisher akkumulierte Nutzung der Session
    ///   (`TokenUsage::total()`). Wird nicht gegen das Fenster gedeckelt —
    ///   ein Überschreiten ist der Normalfall des Triggers.
    /// - `task_completed` (`bool`): `true`, wenn der soeben beendete Turn
    ///   eine Aufgabe abgeschlossen hat (z. B. Plan-Schritt `completed`).
    ///
    /// # Returns
    /// Die Entscheidung. [`CompactDecision::BudgetExceeded`] hat Vorrang vor
    /// [`CompactDecision::TaskCompleted`]; beide Schwellen sind exklusiv
    /// (`>` verglichen, nicht `>=`), damit die genau an der Schwelle
    /// liegende Nutzung noch nicht auslöst.
    ///
    /// Bei deaktivierter Policy (Fenster 0) immer [`CompactDecision::None`].
    #[must_use]
    pub fn decide(&self, tokens_used: u64, task_completed: bool) -> CompactDecision {
        if self.context_window_tokens == 0 {
            return CompactDecision::None;
        }
        if tokens_used > self.effective_compact_threshold_tokens() {
            return CompactDecision::BudgetExceeded;
        }
        if task_completed && tokens_used > self.task_end_threshold_tokens {
            return CompactDecision::TaskCompleted;
        }
        CompactDecision::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn rescaled_keeps_ceiling_and_target() {
        let policy = AutoCompactPolicy::for_context_window(200_000)
            .with_absolute_ceiling(Some(500_000))
            .with_turn_start_target(Some(40_000))
            .rescaled(1_000_000);
        assert_eq!(policy.context_window_tokens(), 1_000_000);
        assert_eq!(policy.turn_start_target_tokens(), Some(40_000));
        assert_eq!(policy.absolute_ceiling_tokens, Some(500_000));
    }

    #[test]
    fn thresholds_derive_from_context_window() {
        let policy = AutoCompactPolicy::for_context_window(200_000);
        assert_eq!(policy.context_window_tokens(), 200_000);
        assert_eq!(policy.compact_threshold_tokens(), 140_000);
        assert_eq!(policy.task_end_threshold_tokens(), 60_000);
    }

    #[test]
    fn thresholds_scale_with_large_windows() {
        let policy = AutoCompactPolicy::for_context_window(1_000_000);
        assert_eq!(policy.compact_threshold_tokens(), 700_000);
        assert_eq!(policy.task_end_threshold_tokens(), 300_000);
    }

    #[test]
    fn disabled_policy_never_compacts() {
        let policy = AutoCompactPolicy::disabled();
        assert_eq!(policy.decide(u64::MAX, false), CompactDecision::None);
        assert_eq!(policy.decide(u64::MAX, true), CompactDecision::None);
    }

    #[test]
    fn below_all_thresholds_no_compact() {
        let policy = AutoCompactPolicy::for_context_window(200_000);
        assert_eq!(policy.decide(50_000, false), CompactDecision::None);
        assert_eq!(policy.decide(50_000, true), CompactDecision::None);
    }

    #[test]
    fn above_compact_threshold_triggers_budget_exceeded() {
        let policy = AutoCompactPolicy::for_context_window(200_000);
        assert_eq!(
            policy.decide(140_001, false),
            CompactDecision::BudgetExceeded,
        );
        // BudgetExceeded hat Vorrang vor TaskCompleted.
        assert_eq!(
            policy.decide(140_001, true),
            CompactDecision::BudgetExceeded,
        );
    }

    #[test]
    fn at_threshold_exactly_does_not_trigger() {
        // Schwellen sind exklusiv: genau 140_000 löst noch nicht aus.
        let policy = AutoCompactPolicy::for_context_window(200_000);
        assert_eq!(policy.decide(140_000, false), CompactDecision::None);
        assert_eq!(policy.decide(60_000, true), CompactDecision::None);
    }

    #[test]
    fn task_completed_above_task_end_threshold_triggers() {
        let policy = AutoCompactPolicy::for_context_window(200_000);
        assert_eq!(policy.decide(60_001, true), CompactDecision::TaskCompleted,);
        // Ohne Task-Ende bleibt es unterhalb des Budget-Deckels ruhig.
        assert_eq!(policy.decide(60_001, false), CompactDecision::None);
    }

    #[test]
    fn task_completed_below_threshold_does_not_trigger() {
        let policy = AutoCompactPolicy::for_context_window(200_000);
        assert_eq!(policy.decide(30_000, true), CompactDecision::None);
    }

    #[test]
    fn saturation_does_not_panic_on_huge_windows() {
        let policy = AutoCompactPolicy::for_context_window(u64::MAX);
        // Keine Panik, keine Überlauf-Wraps.
        assert_eq!(
            policy.decide(u64::MAX, false),
            CompactDecision::BudgetExceeded
        );
        assert!(policy.compact_threshold_tokens() > 0);
    }

    #[test]
    fn decision_should_compact_flag() {
        assert!(!CompactDecision::None.should_compact());
        assert!(CompactDecision::BudgetExceeded.should_compact());
        assert!(CompactDecision::TaskCompleted.should_compact());
    }

    #[test]
    fn default_ceiling_is_500k_input_tokens() {
        // Auto-Compact-Deckel: 500 000 Input-Tokens.
        assert_eq!(DEFAULT_ABSOLUTE_CEILING_TOKENS, 500_000);
    }

    #[test]
    fn absolute_ceiling_lowers_effective_threshold() {
        // 1M-Fenster: relative Schwelle 700k, aber Deckel 500k greift zuerst.
        let policy = AutoCompactPolicy::for_context_window(1_000_000)
            .with_absolute_ceiling(Some(DEFAULT_ABSOLUTE_CEILING_TOKENS));
        assert_eq!(policy.decide(500_000, false), CompactDecision::None);
        assert_eq!(
            policy.decide(500_001, false),
            CompactDecision::BudgetExceeded,
        );
    }

    #[test]
    fn absolute_ceiling_above_relative_threshold_has_no_effect() {
        let policy =
            AutoCompactPolicy::for_context_window(200_000).with_absolute_ceiling(Some(500_000));
        assert_eq!(
            policy.decide(140_001, false),
            CompactDecision::BudgetExceeded,
        );
    }

    #[test]
    fn turn_start_target_builder_roundtrip() {
        let policy = AutoCompactPolicy::for_context_window(200_000)
            .with_turn_start_target(Some(DEFAULT_ORCHESTRATOR_TURN_START_TARGET_TOKENS));
        assert_eq!(
            policy.turn_start_target_tokens(),
            Some(DEFAULT_ORCHESTRATOR_TURN_START_TARGET_TOKENS),
        );
        let cleared = policy.with_turn_start_target(None);
        assert_eq!(cleared.turn_start_target_tokens(), None);
    }

    #[test]
    fn policy_serde_roundtrip() -> TestResult {
        let policy = AutoCompactPolicy::for_context_window(200_000);
        let json = serde_json::to_string(&policy).map_err(ctx("serialize"))?;
        let parsed: AutoCompactPolicy = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(policy, parsed);
        Ok(())
    }
}
