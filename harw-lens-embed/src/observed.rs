//! Beobachtetes Verhalten: was *gemessen* wurde.
//!
//! # Verantwortungsbereich
//! Besitzt [`ObservedBehavior`] -- Trefferqualität auf einem Goldkorpus und
//! Latenz eines Modells, wie sie tatsächlich gemessen wurden. Getrennt von
//! [`crate::spec::EmbeddingSpec`] und [`crate::runtime::RuntimeProfile`]:
//! beide beschreiben eine Erwartung (was das Modell ist / wie es betrieben
//! wird), dieser Typ einen Messwert. Anfangs leer -- diese Crate misst
//! nichts selbst; das Befüllen ist Aufgabe eines nachgelagerten Knotens und
//! kommt deshalb bewusst nicht aus `embeddings.toml`.
//!
//! # Nebenläufigkeit
//! Reine Daten (`Clone`, `Copy`, `PartialEq`), ohne Einschränkung zwischen
//! Threads teilbar.
//!
//! # Fehler
//! Erzeugt selbst keine Fehler.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::ObservedBehavior;
//!
//! let observed = ObservedBehavior::default();
//! assert!(observed.retrieval_quality.is_none());
//! assert!(observed.latency_ms.is_none());
//! ```

/// Was für ein Modell *gemessen* wurde.
///
/// # Description
/// `Default` liefert den anfangs leeren Zustand: kein Modell hat in dieser
/// Crate je einen Goldkorpus-Durchlauf oder eine Latenzmessung durchlaufen.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ObservedBehavior {
    /// Trefferqualität (z. B. nDCG oder Recall@k) auf einem Goldkorpus,
    /// falls bereits gemessen.
    pub retrieval_quality: Option<f32>,
    /// Beobachtete Latenz in Millisekunden (z. B. p50), falls bereits
    /// gemessen.
    pub latency_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observed_behavior_default_is_empty() {
        let observed = ObservedBehavior::default();
        assert_eq!(observed.retrieval_quality, None);
        assert_eq!(observed.latency_ms, None);
    }

    #[test]
    fn test_observed_behavior_can_hold_measured_values() {
        let observed = ObservedBehavior {
            retrieval_quality: Some(0.87),
            latency_ms: Some(12.5),
        };
        assert_eq!(observed.retrieval_quality, Some(0.87));
        assert_eq!(observed.latency_ms, Some(12.5));
    }
}
