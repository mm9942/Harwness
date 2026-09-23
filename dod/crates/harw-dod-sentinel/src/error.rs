//! Fehlertyp von `harw-dod-sentinel` (Knoten AW2-18).
//!
//! # Verantwortungsbereich
//! Diese Crate ruft Sensoren ab, führt einen Degradationsautomaten und
//! puffert Samples/Events — sie deutet aber keine Quelle und hat deshalb nur
//! einen einzigen eigenen Fehlerpfad: [`crate::buffer::EvidenceBuffer::freeze`]
//! (und darüber [`crate::Sentinel::freeze`]) delegieren an
//! [`harw_dod_signals::SecurityEvidence::capture`], das bei einer
//! fehlgeschlagenen kanonischen JSON-Kodierung von Samples und Events einen
//! [`harw_dod_signals::SignalsError`] liefert (siehe dortige Moduldoku:
//! praktisch unerreichbar für die hier gehaltenen Feldtypen, aber die
//! Signatur trägt ihre wahre Fehlbarkeit, statt sie hinter einem `unwrap()`
//! oder `expect()` zu verstecken — beide sind außerhalb von Tests in diesem
//! Arbeitsauftrag ausdrücklich verboten).
//!
//! Fehler eines einzelnen Sensor-Abrufs selbst — Lesefehler, außerhalb des
//! Bereichs, Quelle fehlt — gehören **nicht** hierher: die trägt
//! [`harw_dod_cap::SensorError`], von [`crate::health`] ausgewertet
//! (`permanence()`), nie in diesen Fehlertyp übersetzt. Ein Sensorfehler ist
//! ein normaler, erwarteter Zustand des Automaten (`Retrying`/`Degraded`),
//! kein `Err`-Rückgabewert dieser Crate.
//!
//! # Exportierte Typen
//! [`SentinelError`], sowie der von `#[derive(harw_macros::HarwError)]`
//! erzeugte Alias `SentinelResult<T>`.
//!
//! # Nebenläufigkeit
//! `SentinelError` ist `Send + Sync`, weil sein einziges Feld
//! (`harw_dod_signals::SignalsError`) beides ist. Kein internes Locking,
//! keine geteilten Ressourcen.
//!
//! # Examples
//! ```rust
//! use harw_dod_sentinel::error::SentinelError;
//!
//! fn describe(err: &SentinelError) -> String {
//!     err.to_string()
//! }
//! ```

/// Fehler dieser Crate.
///
/// # Description
/// Ein einziger Fehlerpfad: die kanonische JSON-Kodierung von Samples und
/// Events beim Einfrieren eines Ringpuffers zu [`harw_dod_signals::SecurityEvidence`]
/// kann scheitern (siehe Moduldoku für die Begründung, warum dieser Pfad
/// praktisch unerreichbar, die Signatur aber dennoch `Result`-basiert ist).
///
/// # Errors
/// Diese Variante wird über `?`/`From<harw_dod_signals::SignalsError>`
/// erzeugt, nie von Hand konstruiert.
#[derive(Debug, harw_macros::HarwError)]
pub enum SentinelError {
    /// Das Einfrieren des Ringpuffers zu einem zitierfähigen Beleg ist
    /// fehlgeschlagen. Siehe [`harw_dod_signals::evidence`]-Moduldoku für die
    /// zugrunde liegende Digest-Bildungsregel.
    #[msg("failed to freeze sentinel buffer contents into evidence: {0}")]
    #[from]
    Evidence(harw_dod_signals::SignalsError),
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{SentinelError, SentinelResult};
    use crate::test_support::{TestError, TestResult};

    // Erzeugt einen echten `SignalsError` für die Tests, ohne die
    // Digestbildung selbst anzustoßen: der `#[from]`-Pfad verhält sich
    // unabhängig davon, welcher Aufrufer ihn auslöst.
    fn sample_signals_error() -> TestResult<harw_dod_signals::SignalsError> {
        let Err(json_err) = serde_json::from_str::<serde_json::Value>("not json") else {
            return Err(TestError::Unexpected(
                "deliberately malformed JSON must fail to parse".into(),
            ));
        };
        Ok(harw_dod_signals::SignalsError::from(json_err))
    }

    #[test]
    fn test_evidence_display_includes_prefix_and_inner_message() -> TestResult {
        let err = SentinelError::from(sample_signals_error()?);
        let display = err.to_string();
        assert!(display.starts_with("failed to freeze sentinel buffer contents into evidence:"));
        Ok(())
    }

    #[test]
    fn test_evidence_source_returns_inner_error() -> TestResult {
        let err: SentinelError = sample_signals_error()?.into();
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_sentinel_result_alias_carries_sentinel_error() -> TestResult {
        fn always_fails(signals_error: harw_dod_signals::SignalsError) -> SentinelResult<()> {
            Err(SentinelError::from(signals_error))
        }

        assert!(always_fails(sample_signals_error()?).is_err());
        Ok(())
    }
}
