//! Fehlertyp von `harw-dod-signals` (Contract-Master §G, §H.1; AW6-02 für die
//! `Verdict*`-Varianten).
//!
//! # Verantwortungsbereich
//! Diese Crate deklariert reines Vokabular — Messwerte, Ereignisse, Belege,
//! den [`crate::sensor::Sensor`]-Trait und (seit AW6-02) den Verdict-Vertrag
//! [`crate::verdict::SecurityVerdict`] — und liest selbst keine Quelle. Sie
//! hat deshalb zwei Arten von Fehlerursachen:
//! - **`DigestEncoding`**: die kanonische Serialisierung von Samples und
//!   Events beim Bilden eines [`crate::evidence::SecurityEvidence`]-Belegs
//!   (siehe [`crate::evidence`] für die Digest-Bildungsregel).
//! - **`Verdict*`**: das Parsen, Versionsprüfen, Validieren und
//!   Binden-Prüfen eines vom Kind-Agenten empfangenen
//!   [`crate::verdict::SecurityVerdict`] (siehe [`crate::verdict`] für den
//!   vollständigen Vertrag).
//!
//! Fehler eines Sensors selbst — Lesefehler, außerhalb des Lesebereichs,
//! Quelle nicht vorhanden — gehören **nicht** hierher: die trägt
//! [`harw_dod_cap::SensorError`] (Contract-Master §F), bewusst inhaltsfrei.
//! [`crate::sensor::Sensor::poll`] gibt deshalb `harw_dod_cap::SensorError`
//! zurück, nicht [`SignalsError`].
//!
//! # Inhaltsfreie `Verdict*`-Varianten
//! Alle vier `Verdict*`-Varianten sind absichtlich feldlos: ihre
//! `Display`-Ausgabe nennt **dass** ein Verdikt abgelehnt wurde, nie **was**
//! im Rohtext, im `contract`-Label oder in einem geprüften Feld stand — anders
//! als `DigestEncoding`, das die interne `serde_json`-Meldung über eigene,
//! bereits validierte Daten zitiert (kein Angreifer-Kanal). Ein Verdikt
//! transportiert dagegen Text, den ein Triage-Agent aus angreiferkontrollierten
//! Sensorfeldern abgeleitet haben kann; ihn in eine Fehlermeldung zu zitieren
//! hieße, ihn dorthin durchzureichen, wo er geloggt wird.
//!
//! # Exportierte Typen
//! [`SignalsError`], sowie der von `#[derive(harw_macros::HarwError)]`
//! erzeugte Alias `SignalsResult<T>`.
//!
//! # Nebenläufigkeit
//! `SignalsError` ist `Send + Sync`: sein einziges nicht-triviales Feld
//! (`serde_json::Error` in `DigestEncoding`) ist beides, die `Verdict*`-
//! Varianten sind feldlos. Kein internes Locking, keine geteilten
//! Ressourcen.
//!
//! # Examples
//! ```rust
//! use harw_dod_signals::error::SignalsError;
//!
//! fn describe(err: &SignalsError) -> String {
//!     err.to_string()
//! }
//! ```

/// Fehler dieser Crate.
///
/// # Description
/// Ein einziger Fehlerpfad: die kanonische JSON-Kodierung von Samples und
/// Events beim Bilden eines [`crate::evidence::SecurityEvidence`]-Belegs kann
/// scheitern. Für die heute definierten Feldtypen (Zeichenketten,
/// Ganzzahlen, `f64`, [`harw_types::ContentDigest`], `jiff::Timestamp`) ist
/// dieser Pfad praktisch nicht erreichbar — `serde_json` kodiert auch
/// nicht-endliche Fließkommawerte (`NaN`, `Infinity`) als `null`, statt mit
/// einem Fehler abzubrechen. Der Konstruktor bleibt trotzdem `Result`-
/// basiert: eine Serialisierungsfunktion trägt damit ihre wahre Signatur,
/// und ein künftiges Feld mit einer echt fehlschlagbaren `Serialize`-Impl
/// (etwa eine `HashMap` mit nicht-string-fähigen Schlüsseln) bricht die
/// Aufrufer nicht binär.
///
/// # Errors
/// Diese Variante wird über `?`/`From<serde_json::Error>` erzeugt, nie von
/// Hand konstruiert.
#[derive(Debug, harw_macros::HarwError)]
pub enum SignalsError {
    /// Die kanonische Kodierung der Samples und Events für die Digestbildung
    /// ist fehlgeschlagen. Siehe [`crate::evidence`]-Moduldoku für die
    /// Bildungsregel, die diesen Schritt auslöst.
    #[msg("failed to encode evidence samples and events for digest formation: {0}")]
    #[from]
    DigestEncoding(serde_json::Error),

    /// Der (fence-bereinigte) Kind-Text ist kein gültiges
    /// `SecurityVerdict`-JSON. Inhaltsfrei (siehe Moduldoku): der Rohtext
    /// erscheint nicht in der `Display`-Ausgabe.
    #[msg("the child response is not a well-formed SecurityVerdict")]
    VerdictMalformed,

    /// Das JSON war gültig, aber sein `contract`-Feld war nicht
    /// [`crate::verdict::SecurityVerdict::CONTRACT_ID`] — eine unbekannte
    /// oder andere Vertragsfassung wurde erkannt, nicht geraten (siehe
    /// [`crate::verdict`]-Moduldoku, Abschnitt „Versionierung").
    #[msg("the verdict declares a contract version this receiver does not know")]
    VerdictUnknownContractVersion,

    /// Ein Pflichtfeld des Verdikts (`rationale`, `issued_by`) ist leer oder
    /// nur Whitespace.
    #[msg("the verdict is missing a required field")]
    VerdictEmptyField,

    /// [`crate::verdict::SecurityVerdict::binds`] scheiterte: das Verdikt
    /// ist nicht an den erwarteten Befund und/oder den erwarteten
    /// Belegdigest gebunden. Der wichtigste Fehlerpfad dieses Vertrags —
    /// siehe [`crate::verdict`]-Moduldoku, Abschnitt „Woher der
    /// Vergleichswert kommt".
    #[msg("the verdict is not bound to the expected finding and evidence digest")]
    VerdictBindingMismatch,
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{SignalsError, SignalsResult};
    use crate::test_support::{TestError, TestResult};

    // Erzeugt einen echten `serde_json::Error` für die Tests, ohne die
    // Digestbildung selbst anzustoßen: das Verhalten des `#[from]`-Pfads ist
    // unabhängig davon, welcher Aufrufer ihn auslöst.
    fn sample_json_error() -> TestResult<serde_json::Error> {
        let Err(error) = serde_json::from_str::<serde_json::Value>("not json") else {
            return Err(TestError::Unexpected(
                "expected an Err from invalid JSON".into(),
            ));
        };
        Ok(error)
    }

    #[test]
    fn test_digest_encoding_display_includes_prefix_and_inner_message() -> TestResult {
        let err = SignalsError::from(sample_json_error()?);
        let display = err.to_string();
        assert!(
            display
                .starts_with("failed to encode evidence samples and events for digest formation:")
        );
        Ok(())
    }

    #[test]
    fn test_digest_encoding_source_returns_inner_error() -> TestResult {
        let err: SignalsError = sample_json_error()?.into();
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_signals_result_alias_carries_signals_error() -> TestResult {
        let json_error = sample_json_error()?;
        fn always_fails(json_error: serde_json::Error) -> SignalsResult<()> {
            Err(SignalsError::from(json_error))
        }

        assert!(always_fails(json_error).is_err());
        Ok(())
    }

    // -- Verdict*-Varianten sind inhaltsfrei -----------------------------------

    #[test]
    fn test_verdict_malformed_display_has_no_source_and_is_content_free() {
        let err = SignalsError::VerdictMalformed;
        assert_eq!(
            err.to_string(),
            "the child response is not a well-formed SecurityVerdict"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn test_verdict_unknown_contract_version_display() {
        let err = SignalsError::VerdictUnknownContractVersion;
        assert_eq!(
            err.to_string(),
            "the verdict declares a contract version this receiver does not know"
        );
    }

    #[test]
    fn test_verdict_binding_mismatch_display() {
        let err = SignalsError::VerdictBindingMismatch;
        assert_eq!(
            err.to_string(),
            "the verdict is not bound to the expected finding and evidence digest"
        );
    }
}
