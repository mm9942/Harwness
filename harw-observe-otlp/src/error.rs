//! Fehlertyp für `harw-observe-otlp`.
//!
//! # Verantwortungsbereich
//! Trägt [`OtlpError`], den einen Fehlertyp dieser Crate (Vertrag §H.1,
//! `docs/aw-contract-master.md`). Alle drei Varianten entstehen ausschließlich
//! intern, in `crate::schema` (Zeitumrechnung, JSON-Serialisierung) und
//! [`crate::OtlpTransport`]-Implementierungen (Zustellfehler);
//! [`crate::OtlpSink::record`] und `::flush` geben `()` zurück (Vertrag
//! A.3) und propagieren deshalb nie einen `OtlpError` an einen Aufrufer —
//! jede hier erzeugte Instanz wird crateintern abgefangen und über einen
//! `AtomicU64`-Zähler auf [`crate::OtlpSink`] sichtbar gemacht, nach
//! demselben Muster wie `harw-observe-file::FileSink::write_error_count`
//! und `harw-observe-prom::PromSink::unsupported_histogram_count`.
//!
//! # Nebenläufigkeit
//! `OtlpError` trägt nur `i128`-, `String`- und `serde_json::Error`-Felder,
//! ist `Send + Sync` und ohne Interior Mutability — beliebig zwischen
//! Threads teilbar.
//!
//! # Fehler
//! `Display`, `Debug` (zusätzliche Ableitung), `std::error::Error` und der
//! `OtlpResult<T>`-Alias entstehen über `#[derive(harw_macros::HarwError)]`.
//! Kein `anyhow`, kein `thiserror`.
//!
//! # Examples
//! ```
//! use harw_observe_otlp::OtlpError;
//!
//! let err = OtlpError::TimestampOutOfRange { nanos: -1 };
//! assert!(err.to_string().contains("-1"));
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate (Vertrag Kopfteil „Fehler").
///
/// # Description
/// [`OtlpError::TimestampOutOfRange`] entsteht in
/// `crate::schema::timestamp_to_unix_nanos`, wenn ein `jiff::Timestamp` sich
/// nicht verlustfrei in ein vorzeichenloses 64-Bit-Nanosekunden-Feld (OTLP
/// `fixed64`) umrechnen lässt — vor 1970 (negativ) oder nach etwa dem Jahr
/// 2554 (größer als `u64::MAX`). [`OtlpError::Json`] entsteht beim
/// Serialisieren eines Stapels zu OTLP/JSON (siehe `crate::schema`);
/// `#[from]` erlaubt `?` an der einzigen Erzeugungsstelle.
/// [`OtlpError::Send`] ist der generische Zustellfehler, den eine
/// [`crate::OtlpTransport`]-Implementierung zurückgeben kann —
/// ohne fremden Fehlertyp im Feld, weil eine künftige echte
/// HTTP-Transport-Implementierung sonst einen Pre-1.0-Fremdcrate-Typ (z. B.
/// aus einer HTTP-Bibliothek) in dieses Feld und damit in eine öffentliche
/// Signatur dieser Crate ziehen würde — genau das, wovor die
/// OTel-Adapter-Isolation (siehe Crate-Doc) schützt.
#[derive(Debug, HarwError)]
pub enum OtlpError {
    /// Ein `jiff::Timestamp` lässt sich nicht verlustfrei in OTLPs
    /// vorzeichenloses 64-Bit-Nanosekunden-Feld umrechnen.
    ///
    /// # Arguments
    /// - `nanos` (`i128`): der volle Nanosekundenwert seit der Epoche
    ///   (`jiff::Timestamp::as_nanosecond`), der außerhalb von
    ///   `0..=u64::MAX` liegt.
    #[msg("timestamp {nanos} ns since epoch is outside OTLP's representable u64 nanosecond range")]
    TimestampOutOfRange {
        /// Der volle Nanosekundenwert seit der Epoche, der nicht passt.
        nanos: i128,
    },

    /// Die OTLP/JSON-Serialisierung eines Stapels ist fehlgeschlagen.
    ///
    /// # Arguments
    /// - `0` (`serde_json::Error`): die zugrunde liegende
    ///   Serialisierungsursache.
    #[from]
    Json(serde_json::Error),

    /// Eine [`crate::OtlpTransport`]-Implementierung konnte einen
    /// Stapel nicht zustellen.
    ///
    /// # Arguments
    /// - `reason` (`String`): die vom Transport gemeldete Ursache, im
    ///   Klartext statt als fremder Fehlertyp (siehe `# Description` oben).
    #[msg("OTLP transport failed to deliver a batch: {reason}")]
    Send {
        /// Die vom Transport gemeldete Ursache.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_otlp_error_display_timestamp_out_of_range() {
        let err = OtlpError::TimestampOutOfRange { nanos: -5 };
        assert_eq!(
            err.to_string(),
            "timestamp -5 ns since epoch is outside OTLP's representable u64 nanosecond range"
        );
    }

    #[test]
    fn test_otlp_error_display_send() {
        let err = OtlpError::Send {
            reason: "connection refused".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "OTLP transport failed to deliver a batch: connection refused"
        );
    }

    #[test]
    fn test_otlp_error_from_json_error_and_source_is_some() -> TestResult {
        use std::error::Error as _;
        let Err(json_err) = serde_json::from_str::<serde_json::Value>("{not json") else {
            return Err(TestError::Unexpected(
                "malformed JSON must fail to parse".to_owned(),
            ));
        };
        let err: OtlpError = json_err.into();
        assert!(matches!(err, OtlpError::Json(_)));
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_otlp_error_timestamp_out_of_range_source_is_none() {
        use std::error::Error as _;
        let err = OtlpError::TimestampOutOfRange { nanos: 1 };
        assert!(err.source().is_none());
    }

    #[test]
    fn test_otlp_result_alias_exists() -> TestResult {
        fn make() -> OtlpResult<u8> {
            Ok(1)
        }
        assert_eq!(
            make().map_err(ctx("OtlpResult alias always succeeds here"))?,
            1
        );
        Ok(())
    }
}
