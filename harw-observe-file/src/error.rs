//! Fehlertyp für `harw-observe-file`.
//!
//! # Verantwortungsbereich
//! Trägt [`ObserveFileError`], den einen Fehlertyp dieser Crate (Vertrag
//! §H.1, `docs/design/build-history.md`). Deckt Verzeichnis-/Dateizugriff
//! beim Öffnen ([`FileSink::open`][crate::FileSink::open]) und beim
//! Rotieren der aktiven Datei ab.
//!
//! # Nebenläufigkeit
//! `ObserveFileError` ist `Send + Sync` (nur `String`- und
//! `std::io::Error`-Felder) und ohne Interior Mutability.
//!
//! # Fehler
//! `Display`, `Debug` (Standardableitung) und `std::error::Error` (ohne
//! `source()`-Verkettung — beide Varianten tragen zusätzlichen
//! `path`-Kontext neben der I/O-Ursache, `#[from]` verlangt aber eine
//! Tupel-Variante mit genau einem Feld) sowie der
//! `ObserveFileResult<T>`-Alias entstehen über
//! `#[derive(harw_macros::HarwError)]`. Kein `anyhow`, kein `thiserror`.
//!
//! # Examples
//! ```
//! use harw_observe_file::ObserveFileError;
//!
//! let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
//! let err = ObserveFileError::Open {
//!     path: "/no/such/dir".to_owned(),
//!     source,
//! };
//! assert!(err.to_string().contains("/no/such/dir"));
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate.
///
/// # Description
/// Beide Varianten tragen den betroffenen Pfad als `String` (nicht
/// `PathBuf`, das keine `Display`-Implementierung hat und damit nicht in
/// ein `#[msg(...)]`-Format eingesetzt werden könnte — Muster:
/// `harw-code-graph/src/error.rs`) neben der zugrunde liegenden
/// `std::io::Error`-Ursache.
#[derive(Debug, HarwError)]
pub enum ObserveFileError {
    /// Das Zielverzeichnis oder die aktive Datei kann nicht angelegt,
    /// geöffnet oder gelesen werden.
    ///
    /// # Arguments
    /// - `path` (`String`): das betroffene Verzeichnis oder die betroffene
    ///   Datei.
    /// - `source` (`std::io::Error`): die zugrunde liegende I/O-Ursache.
    #[msg("telemetry directory '{path}' is not writable: {source}")]
    Open {
        /// Das betroffene Verzeichnis oder die betroffene Datei.
        path: String,
        /// Die zugrunde liegende I/O-Ursache.
        source: std::io::Error,
    },

    /// Die aktive Datei konnte nicht rotiert werden (umbenennen, Digest
    /// berechnen oder die `.blake3`-Beidatei schreiben).
    ///
    /// # Arguments
    /// - `path` (`String`): die betroffene Datei.
    /// - `source` (`std::io::Error`): die zugrunde liegende I/O-Ursache.
    #[msg("failed to rotate telemetry file '{path}': {source}")]
    Rotate {
        /// Die betroffene Datei.
        path: String,
        /// Die zugrunde liegende I/O-Ursache.
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn io_error() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied")
    }

    #[test]
    fn test_observe_file_error_open_display_contains_path_and_source() {
        let err = ObserveFileError::Open {
            path: "/var/lib/harwness".to_owned(),
            source: io_error(),
        };
        let message = err.to_string();
        assert!(message.contains("/var/lib/harwness"));
        assert!(message.contains("denied"));
    }

    #[test]
    fn test_observe_file_error_rotate_display_contains_path_and_source() {
        let err = ObserveFileError::Rotate {
            path: "active.jsonl".to_owned(),
            source: io_error(),
        };
        let message = err.to_string();
        assert!(message.contains("active.jsonl"));
        assert!(message.contains("denied"));
    }

    #[test]
    fn test_observe_file_error_source_is_none_for_named_field_variants() {
        use std::error::Error as _;
        let err = ObserveFileError::Open {
            path: "x".to_owned(),
            source: io_error(),
        };
        // `#[from]` verlangt eine Tupel-Variante mit genau einem Feld
        // (harw-macros/src/error.rs); benannte Felder mit zusätzlichem
        // Kontext (hier `path`) verketten `source()` deshalb nicht.
        assert!(err.source().is_none());
    }

    #[test]
    fn test_observe_file_result_alias_exists() -> TestResult {
        fn make() -> ObserveFileResult<u8> {
            Ok(1)
        }
        assert_eq!(make().map_err(ctx("observe file result alias"))?, 1);
        Ok(())
    }
}
