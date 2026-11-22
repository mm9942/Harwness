//! Agent-Budget: eigenes Kontextfenster des Modells plus Schwelle fuer den
//! geordneten Abschluss.
//!
//! Das Budget eines Agenten ist das Kontextfenster seines Modells (optional
//! durch einen deklarierten Wert verengt, nie erweitert), abzueglich einer
//! Reserve fuer die Ausgabe. Erreicht die Fuellung des Kontexts die
//! Abschluss-Schwelle (Standard 85 Prozent), erhaelt der Agent eine
//! Abschlussrunde, in der er an einem konsistenten Punkt stoppt und eine
//! Uebergabe schreibt. Bei der harten Grenze (Standard 95 Prozent) wird der
//! Zug abgebrochen. Das Modul ist rein (keine I/O, keine Abhaengigkeiten).

/// Standard-Schwelle der Abschlussrunde in Prozent des nutzbaren Kontexts.
pub const DEFAULT_GRACEFUL_SHUTDOWN_PERCENT: u64 = 85;
/// Standard-Grenze des harten Abbruchs in Prozent des nutzbaren Kontexts.
pub const DEFAULT_HARD_CONTEXT_LIMIT_PERCENT: u64 = 95;

/// Prozentwerte fuer Abschlussrunde und harte Grenze.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GracefulShutdownPolicy {
    graceful_percent: u64,
    hard_percent: u64,
}

impl GracefulShutdownPolicy {
    /// # Beschreibung
    /// Erzeugt eine normalisierte Richtlinie: hart wird auf 1..=100
    /// begrenzt, die Abschluss-Schwelle auf 1..=hart.
    ///
    /// # Returns
    /// Die normalisierte Richtlinie.
    #[must_use]
    pub const fn new(graceful_percent: u64, hard_percent: u64) -> Self {
        let hard = if hard_percent < 1 {
            1
        } else if hard_percent > 100 {
            100
        } else {
            hard_percent
        };
        let graceful = if graceful_percent < 1 {
            1
        } else if graceful_percent > hard {
            hard
        } else {
            graceful_percent
        };
        Self {
            graceful_percent: graceful,
            hard_percent: hard,
        }
    }

    /// # Beschreibung
    /// Liefert die Schwelle der Abschlussrunde in Prozent.
    ///
    /// # Returns
    /// Prozentwert 1..=hard.
    #[must_use]
    pub const fn graceful_percent(&self) -> u64 {
        self.graceful_percent
    }

    /// # Beschreibung
    /// Liefert die Grenze des harten Abbruchs in Prozent.
    ///
    /// # Returns
    /// Prozentwert 1..=100.
    #[must_use]
    pub const fn hard_percent(&self) -> u64 {
        self.hard_percent
    }

    /// # Beschreibung
    /// Rechnet die Abschluss-Schwelle auf ein Token-Limit um.
    ///
    /// # Returns
    /// `limit * graceful / 100`, saturierend.
    #[must_use]
    pub fn graceful_threshold_of(&self, limit: u64) -> u64 {
        percent_of(limit, self.graceful_percent)
    }

    /// # Beschreibung
    /// Rechnet die harte Grenze auf ein Token-Limit um.
    ///
    /// # Returns
    /// `limit * hard / 100`, saturierend.
    #[must_use]
    pub fn hard_limit_of(&self, limit: u64) -> u64 {
        percent_of(limit, self.hard_percent)
    }
}

impl Default for GracefulShutdownPolicy {
    fn default() -> Self {
        Self::new(
            DEFAULT_GRACEFUL_SHUTDOWN_PERCENT,
            DEFAULT_HARD_CONTEXT_LIMIT_PERCENT,
        )
    }
}

fn percent_of(limit: u64, percent: u64) -> u64 {
    let scaled = u128::from(limit) * u128::from(percent) / 100;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// Ausloeser einer Abschlussrunde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GracefulShutdownCause {
    /// Kontextfuellung hat die Abschluss-Schwelle erreicht.
    ContextFill,
    /// Das Werkzeugaufruf-Budget ist fast aufgebraucht.
    ToolCalls,
    /// Die Wandzeit-Frist ist erreicht.
    WallTime,
}

impl GracefulShutdownCause {
    /// # Beschreibung
    /// Stabile Kennung fuer Logs und Stop-Details.
    ///
    /// # Returns
    /// `context_fill`, `tool_calls` oder `wall_time`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContextFill => "context_fill",
            Self::ToolCalls => "tool_calls",
            Self::WallTime => "wall_time",
        }
    }
}

/// Aufgeloestes Kontext-Budget eines Agenten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentContextBudget {
    /// Kontextfenster des Modells in Tokens.
    pub window_tokens: u64,
    /// Reserve fuer die Ausgabe in Tokens.
    pub reserve_tokens: u64,
    /// Obergrenze: Fenster, ggf. durch deklarierten Wert verengt.
    pub cap_tokens: u64,
    /// Ein deklarierter Wert ueber dem Fenster wurde ignoriert.
    pub widening_ignored: bool,
}

impl AgentContextBudget {
    /// # Beschreibung
    /// Nutzbare Tokens: Obergrenze minus Reserve (Reserve hoechstens die
    /// Haelfte der Obergrenze).
    ///
    /// # Returns
    /// Nutzbare Token-Anzahl.
    #[must_use]
    pub fn usable_tokens(&self) -> u64 {
        self.cap_tokens
            .saturating_sub(self.reserve_tokens.min(self.cap_tokens / 2))
    }

    /// # Beschreibung
    /// Baut das Zug-Budget aus den nutzbaren Tokens und der Richtlinie.
    ///
    /// # Returns
    /// Das `TurnContextBudget`.
    #[must_use]
    pub fn turn_budget(&self, policy: GracefulShutdownPolicy) -> TurnContextBudget {
        TurnContextBudget::new(self.usable_tokens(), policy)
    }
}

/// # Beschreibung
/// Loest das Kontext-Budget aus Fenster, Reserve und optional deklariertem
/// Maximum auf. Ein deklarierter Wert darf nur verengen.
///
/// # Returns
/// Das aufgeloeste `AgentContextBudget`.
#[must_use]
pub fn resolve_context_budget(
    window_tokens: u64,
    reserve_tokens: u64,
    declared_max_tokens: Option<u64>,
) -> AgentContextBudget {
    AgentContextBudget {
        window_tokens,
        reserve_tokens,
        cap_tokens: declared_max_tokens.map_or(window_tokens, |d| d.min(window_tokens)),
        widening_ignored: declared_max_tokens.is_some_and(|d| d > window_tokens),
    }
}

/// Kontext-Budget eines einzelnen Zugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnContextBudget {
    /// Nutzbare Tokens.
    pub usable_tokens: u64,
    /// Richtlinie fuer Abschluss und harte Grenze.
    pub policy: GracefulShutdownPolicy,
}

impl TurnContextBudget {
    /// # Beschreibung
    /// Erzeugt das Zug-Budget.
    ///
    /// # Returns
    /// Das `TurnContextBudget`.
    #[must_use]
    pub const fn new(usable_tokens: u64, policy: GracefulShutdownPolicy) -> Self {
        Self {
            usable_tokens,
            policy,
        }
    }

    /// # Beschreibung
    /// Token-Schwelle der Abschlussrunde.
    ///
    /// # Returns
    /// Schwelle in Tokens.
    #[must_use]
    pub fn graceful_threshold_tokens(&self) -> u64 {
        self.policy.graceful_threshold_of(self.usable_tokens)
    }

    /// # Beschreibung
    /// Token-Grenze des harten Abbruchs.
    ///
    /// # Returns
    /// Grenze in Tokens.
    #[must_use]
    pub fn hard_limit_tokens(&self) -> u64 {
        self.policy.hard_limit_of(self.usable_tokens)
    }

    /// # Beschreibung
    /// Bewertet die Kontextfuellung (Grenzen inklusive). Ohne nutzbare
    /// Tokens ist das Urteil immer `Below`.
    ///
    /// # Returns
    /// `Hard`, `Graceful` oder `Below`.
    #[must_use]
    pub fn verdict(&self, fill_tokens: u64) -> ContextFillVerdict {
        if self.usable_tokens == 0 {
            ContextFillVerdict::Below
        } else if fill_tokens >= self.hard_limit_tokens() {
            ContextFillVerdict::Hard
        } else if fill_tokens >= self.graceful_threshold_tokens() {
            ContextFillVerdict::Graceful
        } else {
            ContextFillVerdict::Below
        }
    }
}

/// Urteil ueber die Kontextfuellung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFillVerdict {
    /// Unterhalb der Abschluss-Schwelle.
    Below,
    /// Abschluss-Schwelle erreicht.
    Graceful,
    /// Harte Grenze erreicht.
    Hard,
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn budget_is_the_window_of_the_model() -> TestResult {
        let b = resolve_context_budget(1_000_000, 16_384, None);
        assert_eq!(b.cap_tokens, 1_000_000);
        assert!(!b.widening_ignored);
        Ok(())
    }

    #[test]
    fn a_narrowing_declared_value_wins() -> TestResult {
        let b = resolve_context_budget(1_000_000, 16_384, Some(60_000));
        assert_eq!(b.cap_tokens, 60_000);
        assert!(!b.widening_ignored);
        Ok(())
    }

    #[test]
    fn a_widening_declared_value_is_ignored() -> TestResult {
        let b = resolve_context_budget(200_000, 16_384, Some(2_000_000));
        assert_eq!(b.cap_tokens, 200_000);
        assert!(b.widening_ignored);
        Ok(())
    }

    #[test]
    fn usable_subtracts_the_output_reserve() -> TestResult {
        assert_eq!(
            resolve_context_budget(100_000, 16_000, None).usable_tokens(),
            84_000
        );
        assert_eq!(resolve_context_budget(1_000, 900, None).usable_tokens(), 500);
        assert_eq!(resolve_context_budget(0, 10, None).usable_tokens(), 0);
        Ok(())
    }

    #[test]
    fn default_policy_is_85_95() -> TestResult {
        let p = GracefulShutdownPolicy::default();
        assert_eq!((p.graceful_percent(), p.hard_percent()), (85, 95));
        let p = GracefulShutdownPolicy::new(99, 50);
        assert_eq!((p.graceful_percent(), p.hard_percent()), (50, 50));
        let p = GracefulShutdownPolicy::new(0, 0);
        assert_eq!((p.graceful_percent(), p.hard_percent()), (1, 1));
        let p = GracefulShutdownPolicy::new(150, 150);
        assert_eq!((p.graceful_percent(), p.hard_percent()), (100, 100));
        assert_eq!(p.hard_limit_of(u64::MAX), u64::MAX);
        Ok(())
    }

    #[test]
    fn verdict_thresholds_are_inclusive() -> TestResult {
        let t = TurnContextBudget::new(1000, GracefulShutdownPolicy::default());
        assert_eq!(t.verdict(849), ContextFillVerdict::Below);
        assert_eq!(t.verdict(850), ContextFillVerdict::Graceful);
        assert_eq!(t.verdict(949), ContextFillVerdict::Graceful);
        assert_eq!(t.verdict(950), ContextFillVerdict::Hard);
        let zero = TurnContextBudget::new(0, GracefulShutdownPolicy::default());
        assert_eq!(zero.verdict(10), ContextFillVerdict::Below);
        let b = resolve_context_budget(1000, 0, None).turn_budget(GracefulShutdownPolicy::default());
        assert_eq!((b.graceful_threshold_tokens(), b.hard_limit_tokens()), (850, 950));
        Ok(())
    }

    #[test]
    fn as_str_labels() -> TestResult {
        assert_eq!(GracefulShutdownCause::ContextFill.as_str(), "context_fill");
        assert_eq!(GracefulShutdownCause::ToolCalls.as_str(), "tool_calls");
        assert_eq!(GracefulShutdownCause::WallTime.as_str(), "wall_time");
        Ok(())
    }
}
