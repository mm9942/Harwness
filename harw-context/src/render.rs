//! Rendering-Vokabular: Detailgrad und Auslassungsgründe.
//!
//! # Verantwortungsbereich
//! Besitzt [`DetailMode`] und [`OmissionReason`] — reine Aufzählungen ohne
//! Verhalten. Wie eine Sektion tatsächlich gerendert wird und wann ein
//! Fragment tatsächlich ausgelassen wird, entscheidet die Montage in
//! `harw-core` (`Assembly<...>`, Knoten AW1-03), nicht diese Crate.
//!
//! # Exportierte Typen
//! [`DetailMode`], [`OmissionReason`].
//!
//! # Nebenläufigkeit
//! Reine Werttypen ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Fehler
//! Keine — beide Typen sind reine geschlossene Aufzählungen ohne fallible
//! Konstruktion.
//!
//! # Examples
//! ```rust
//! use harw_context::{DetailMode, OmissionReason};
//!
//! let mode = DetailMode::Summary;
//! let reason = OmissionReason::OverBudget;
//! assert_ne!(mode, DetailMode::Full);
//! assert_eq!(reason, OmissionReason::OverBudget);
//! ```

/// Wie ausführlich eine Sektion gerendert wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetailMode {
    Full,
    Summary,
    References,
}

/// Warum ein Fragment nicht im Ergebnis steht. Eine Auslassung ohne Grund
/// gibt es nicht.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OmissionReason {
    OverBudget,
    BelowCeiling,
    ExcludedByProgram,
    Superseded,
}

#[cfg(test)]
mod tests {
    use super::{DetailMode, OmissionReason};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_detail_mode_variants_are_pairwise_distinct() {
        assert_ne!(DetailMode::Full, DetailMode::Summary);
        assert_ne!(DetailMode::Summary, DetailMode::References);
        assert_ne!(DetailMode::Full, DetailMode::References);
    }

    #[test]
    fn test_omission_reason_serde_roundtrip() -> TestResult {
        for reason in [
            OmissionReason::OverBudget,
            OmissionReason::BelowCeiling,
            OmissionReason::ExcludedByProgram,
            OmissionReason::Superseded,
        ] {
            let json = serde_json::to_string(&reason).map_err(ctx("reason must serialize"))?;
            let restored: OmissionReason =
                serde_json::from_str(&json).map_err(ctx("reason must deserialize"))?;
            assert_eq!(reason, restored);
        }
        Ok(())
    }

    #[test]
    fn test_detail_mode_serializes_kebab_case() -> TestResult {
        assert_eq!(
            serde_json::to_string(&DetailMode::References)
                .map_err(ctx("detail mode must serialize"))?,
            "\"references\""
        );
        Ok(())
    }
}
