//! Reasoning-Effort-Level für Modell-Requests.
//!
//! Steuert, wie viel interne Denkarbeit ein Modell vor der Antwort investieren
//! darf (Anthropic Extended Thinking `budget_tokens`, OpenAI
//! `reasoning.effort`). Reines Daten-Enum ohne Provider-Bezug — die Übersetzung
//! in provider-spezifische Wire-Werte lebt in `harw-provider`/`harw-provider-http`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Wie viel Denkaufwand ein Modell vor der Antwort investieren darf.
///
/// Monoton ordbar (`Minimal < Low < Medium < High < Xhigh < Max`) — relevant für die
/// Vererbung an Subagent-Spawns, die den Effort-Level des Parents nie
/// überschreiten dürfen, außer bei explizitem Owner-Override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        };
        f.write_str(s)
    }
}

/// Fehler beim Parsen eines [`ReasoningEffort`] aus einem String.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseReasoningEffortError(String);

impl fmt::Display for ParseReasoningEffortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unbekanntes Reasoning-Effort-Level '{}' (erwartet: minimal, low, medium, high, xhigh, max)",
            self.0
        )
    }
}

impl std::error::Error for ParseReasoningEffortError {}

impl FromStr for ReasoningEffort {
    type Err = ParseReasoningEffortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            other => Err(ParseReasoningEffortError(other.to_owned())),
        }
    }
}

impl ReasoningEffort {
    /// Kappt `self` monoton auf höchstens `ceiling` — für die
    /// Subagent-Vererbung (Kind darf den Parent-Effort nie überschreiten,
    /// sofern kein expliziter Override greift).
    #[must_use]
    pub fn clamp_to(self, ceiling: Self) -> Self {
        self.min(ceiling)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_accepts_known_values() {
        assert_eq!(
            "low".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Low
        );
        assert_eq!(
            "HIGH".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::High
        );
        assert_eq!(
            "XHIGH".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Xhigh
        );
        assert_eq!(
            "max".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Max
        );
    }

    #[test]
    fn test_from_str_rejects_unknown_value() {
        assert!("extreme".parse::<ReasoningEffort>().is_err());
    }

    #[test]
    fn test_display_round_trips_through_from_str() {
        for effort in [
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
            ReasoningEffort::Max,
        ] {
            let text = effort.to_string();
            assert_eq!(text.parse::<ReasoningEffort>().unwrap(), effort);
        }
    }

    #[test]
    fn test_serde_uses_stable_snake_case_wire_values() {
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Xhigh).unwrap(),
            "\"xhigh\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Max).unwrap(),
            "\"max\""
        );
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"xhigh\"").unwrap(),
            ReasoningEffort::Xhigh
        );
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"max\"").unwrap(),
            ReasoningEffort::Max
        );
    }

    #[test]
    fn test_ordering_is_monotonic() {
        assert!(ReasoningEffort::Minimal < ReasoningEffort::Low);
        assert!(ReasoningEffort::Low < ReasoningEffort::Medium);
        assert!(ReasoningEffort::Medium < ReasoningEffort::High);
        assert!(ReasoningEffort::High < ReasoningEffort::Xhigh);
        assert!(ReasoningEffort::Xhigh < ReasoningEffort::Max);
    }

    #[test]
    fn test_clamp_to_caps_at_ceiling() {
        assert_eq!(
            ReasoningEffort::High.clamp_to(ReasoningEffort::Low),
            ReasoningEffort::Low
        );
        assert_eq!(
            ReasoningEffort::Low.clamp_to(ReasoningEffort::High),
            ReasoningEffort::Low
        );
    }
}
