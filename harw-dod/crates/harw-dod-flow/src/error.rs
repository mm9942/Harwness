//! Fehler dieser Crate: bewusst inhaltsfrei, wie `harw_dod_bpf::error::BpfError`.
//!
//! # Verantwortungsbereich
//! [`FlowError`] ist der eine Fehlertyp dieser Crate. Er deckt ausschließlich
//! den Formungsteil ab ([`crate::event::parse_flow_payload`]): ein zu kurzer
//! Puffer, ein unbekannter Protokoll-/Richtungs-/Familienwert. Jede Variante
//! trägt genau so viel Kontext, wie zum Unterscheiden der Fälle nötig ist —
//! keine Rohbytes, kein Feldwert, der auf den Inhalt des Puffers schließen
//! ließe.
//!
//! `Display`, `Debug`, `std::error::Error` entstehen über
//! `#[derive(harw_macros::HarwError)]` (Muster: `harw-dod-bpf/src/error.rs`).
//! Kein `anyhow`, kein `thiserror`.
//!
//! # Exportierte Typen
//! [`FlowError`], [`FlowResult`] (vom Makro erzeugter Typalias).
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_flow::error::FlowError;
//!
//! let err = FlowError::MalformedEvent;
//! assert_eq!(err.to_string(), "flow event payload is malformed");
//! ```

use harw_macros::HarwError;

/// Fehler beim Deuten eines Flow-Payloads.
///
/// # Description
/// Diese Crate hat genau eine Fehlerquelle: [`crate::event::parse_flow_payload`]
/// auf einem zu kurzen oder unerwartet geformten Puffer (unbekannter
/// Protokoll-, Richtungs- oder Adressfamilienwert). **Inhaltsfrei:** kein
/// Feldwert, keine Länge, kein Byte des betroffenen Puffers erscheint in der
/// Meldung — Kernel-Ringpuffer-Inhalte verlassen diese Crate nie über eine
/// Fehlermeldung.
#[derive(Debug, HarwError)]
pub enum FlowError {
    /// Der `payload`-Rest eines [`harw_dod_bpf::RawBpfEvent`] hat nicht die
    /// erwartete Form: kürzer als das feste, 32 Byte lange Flow-Layout
    /// (siehe [`crate::event`]-Moduldoku), oder ein Protokoll-/Richtungs-/
    /// Familienbyte außerhalb der bekannten Werte.
    #[msg("flow event payload is malformed")]
    MalformedEvent,
}

#[cfg(test)]
mod tests {
    use super::FlowError;

    #[test]
    fn test_malformed_event_display_is_exact_and_content_free() {
        let err = FlowError::MalformedEvent;
        assert_eq!(err.to_string(), "flow event payload is malformed");
        // Inhaltsfrei: fester String ohne interpolierte Felder — kann
        // strukturell keine Rohbytes enthalten.
    }

    #[test]
    fn test_malformed_event_has_no_source() {
        assert!(std::error::Error::source(&FlowError::MalformedEvent).is_none());
    }

    #[test]
    fn test_flow_result_alias_is_usable() {
        fn always_fails() -> super::FlowResult<()> {
            Err(FlowError::MalformedEvent)
        }
        assert!(always_fails().is_err());
    }
}
