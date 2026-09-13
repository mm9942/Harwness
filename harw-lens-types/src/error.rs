//! Fehlertyp für `harw-lens-types`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`LensTypesError`] für alle Validierungsfehler dieser
//! Crate. Es entstehen ausschließlich Validierungsfehler über bereits
//! vorhandene Werte — diese Crate tut kein I/O und braucht deshalb keine
//! `#[from]`-Varianten für fremde Fehlertypen.
//!
//! `Display`, die `Debug`-Delegation und `std::error::Error` werden nicht von
//! Hand geschrieben, sondern von `#[derive(harw_macros::HarwError)]` erzeugt
//! (Muster: `harw-plan/src/error.rs`, `harw-provider/src/error.rs`). Kein
//! `anyhow`, kein `thiserror`. Weil der Enum-Name auf `Error` endet, erzeugt
//! das Makro außerdem den Typalias `LensTypesResult<T>`.
//!
//! Exportierte Typen: [`LensTypesError`], [`LensTypesResult`].
//!
//! # Nebenläufigkeit
//! `LensTypesError` ist reine Daten (`Clone`, `PartialEq`, `Eq`) und ohne
//! Einschränkung zwischen Threads teilbar.
//!
//! # Examples
//! ```rust
//! use harw_lens_types::{ByteSpan, LensTypesError};
//!
//! let result = ByteSpan::new(4, 0);
//! assert_eq!(result, Err(LensTypesError::InvalidSpan { start: 4, end: 0 }));
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate.
///
/// # Description
/// Reine Validierungsfehler über bereits vorhandene Werte. Jede Variante
/// trägt den vollständigen Kontext, der zum Verstehen des Fehlers ohne
/// Quellcode-Lektüre nötig ist.
#[derive(Debug, Clone, PartialEq, Eq, HarwError)]
pub enum LensTypesError {
    /// Ein [`crate::ByteSpan`] mit `start > end`.
    ///
    /// # Arguments
    /// - `start` (`usize`): der übergebene Anfang.
    /// - `end` (`usize`): das übergebene Ende.
    #[msg("byte span start {start} is greater than end {end}")]
    InvalidSpan {
        /// Der übergebene Anfang des Bereichs.
        start: usize,
        /// Das übergebene Ende des Bereichs.
        end: usize,
    },

    /// Ein [`crate::ByteSpan`] reicht über das Ende des zugehörigen Textes
    /// hinaus.
    ///
    /// # Arguments
    /// - `start` (`usize`): der Anfang des geprüften Bereichs.
    /// - `end` (`usize`): das Ende des geprüften Bereichs.
    /// - `len` (`usize`): die tatsächliche Länge des Textes in Bytes.
    #[msg("byte span {start}..{end} exceeds text length {len}")]
    SpanOutOfBounds {
        /// Der Anfang des geprüften Bereichs.
        start: usize,
        /// Das Ende des geprüften Bereichs.
        end: usize,
        /// Die tatsächliche Länge des Textes in Bytes.
        len: usize,
    },

    /// Zwei [`crate::IndexManifest`] weichen in einem Feld ab, gegen das eine
    /// Abfrage nicht stillschweigend beantwortet werden darf.
    ///
    /// # Arguments
    /// - `field` (`&'static str`): der Name des abweichenden Feldes.
    #[msg("index manifests differ in field '{field}'")]
    ManifestMismatch {
        /// Name des ersten Feldes, in dem die Manifeste voneinander abweichen.
        field: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lens_types_error_display_invalid_span() {
        let err = LensTypesError::InvalidSpan { start: 4, end: 0 };
        assert_eq!(err.to_string(), "byte span start 4 is greater than end 0");
    }

    #[test]
    fn test_lens_types_error_display_span_out_of_bounds() {
        let err = LensTypesError::SpanOutOfBounds {
            start: 0,
            end: 10,
            len: 5,
        };
        assert_eq!(err.to_string(), "byte span 0..10 exceeds text length 5");
    }

    #[test]
    fn test_lens_types_error_display_manifest_mismatch() {
        let err = LensTypesError::ManifestMismatch { field: "model" };
        assert_eq!(err.to_string(), "index manifests differ in field 'model'");
    }
}
