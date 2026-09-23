//! Fehlertyp für `harw-observe`.
//!
//! # Verantwortungsbereich
//! Trägt [`ObserveError`], den einen Fehlertyp dieser Crate (Vertrag §H.1,
//! `docs/aw-contract-master.md`). `harw-observe` hat kein Backend und kein
//! I/O — die Quellen solcher Fehler sind aktuell die validierenden
//! Konstruktoren von [`crate::trace::TraceContext`] sowie
//! [`crate::null_counter::assert_all_zero`] (Knoten AW1-07), wenn ein
//! Nullzähler über null steht. Konsumenten mit eigenem I/O (z. B.
//! `harw-observe-file`) definieren ihren eigenen Fehlertyp, statt diesen
//! hier zu erweitern.
//!
//! # Nebenläufigkeit
//! `ObserveError` trägt nur `String`-Felder, ist `Send + Sync` und ohne
//! Interior Mutability — beliebig zwischen Threads teilbar.
//!
//! # Fehler
//! `Display`, `Debug` (Standardableitung), `std::error::Error` und der
//! `ObserveResult<T>`-Alias entstehen über
//! `#[derive(harw_macros::HarwError)]`. Kein `anyhow`, kein `thiserror`.
//!
//! # Examples
//! ```
//! use harw_observe::{ObserveError, ObserveResult};
//!
//! fn validate(trace_id: &str) -> ObserveResult<()> {
//!     if trace_id.len() != 32 {
//!         return Err(ObserveError::InvalidTraceId {
//!             value: trace_id.to_owned(),
//!         });
//!     }
//!     Ok(())
//! }
//!
//! assert!(validate("too-short").is_err());
//! assert!(validate(&"a".repeat(32)).is_ok());
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate (Vertrag Kopfteil „Fehler").
///
/// # Description
/// `Display`, `std::error::Error` (inklusive `source()`) und der
/// `ObserveResult<T>`-Typalias entstehen über
/// `#[derive(harw_macros::HarwError)]`; `Debug` kommt aus der zusätzlichen
/// `#[derive(Debug, ...)]` (Muster: `harw-plan/src/error.rs`). Alle
/// Varianten tragen keinen fremden Fehlertyp und brauchen deshalb kein
/// `#[from]`.
#[derive(Debug, HarwError)]
pub enum ObserveError {
    /// `trace_id` ist keine 32-stellige Kleinbuchstaben-Hexzeichenkette.
    ///
    /// # Arguments
    /// - `value` (`String`): der abgelehnte Rohwert.
    #[msg("trace_id '{value}' is not 32 lowercase hex characters")]
    InvalidTraceId {
        /// Der abgelehnte Rohwert.
        value: String,
    },

    /// `span_id` (oder `parent_span_id`) ist keine 16-stellige
    /// Kleinbuchstaben-Hexzeichenkette.
    ///
    /// # Arguments
    /// - `value` (`String`): der abgelehnte Rohwert.
    #[msg("span_id '{value}' is not 16 lowercase hex characters")]
    InvalidSpanId {
        /// Der abgelehnte Rohwert.
        value: String,
    },

    /// Ein [`crate::null_counter::NullCounter`] steht über null: die
    /// Invariante, die er bewacht, ist verletzt.
    ///
    /// # Description
    /// Entsteht ausschließlich in
    /// [`crate::null_counter::assert_all_zero`]. Trägt alle drei Angaben,
    /// die zum Verstehen der Verletzung ohne Quellcode-Lektüre nötig sind:
    /// **welcher** Zähler, mit **welchem** Stand, und **welche** Invariante
    /// — eine Meldung „ein Nullzähler ist nicht null" allein wäre wertlos.
    ///
    /// # Arguments
    /// - `name` (`&'static str`): Metrikname des verletzten Zählers.
    /// - `count` (`u64`): sein aktueller Stand (`> 0`).
    /// - `invariant` (`&'static str`): die verletzte Invariante im
    ///   Klartext.
    #[msg("null counter '{name}' is at {count} (expected zero); invariant: {invariant}")]
    NullCounterViolated {
        /// Metrikname des verletzten Zählers.
        name: &'static str,
        /// Sein aktueller Stand (`> 0`).
        count: u64,
        /// Die verletzte Invariante im Klartext.
        invariant: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_observe_error_display_invalid_trace_id() {
        let err = ObserveError::InvalidTraceId {
            value: "xy".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "trace_id 'xy' is not 32 lowercase hex characters"
        );
    }

    #[test]
    fn test_observe_error_display_invalid_span_id() {
        let err = ObserveError::InvalidSpanId {
            value: "xy".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "span_id 'xy' is not 16 lowercase hex characters"
        );
    }

    #[test]
    fn test_observe_error_source_is_none() {
        use std::error::Error as _;
        let err = ObserveError::InvalidTraceId {
            value: "xy".to_owned(),
        };
        assert!(err.source().is_none());
    }

    #[test]
    fn test_observe_result_alias_exists() -> TestResult {
        fn make() -> ObserveResult<u8> {
            Ok(1)
        }
        assert_eq!(make().map_err(ctx("observe result alias"))?, 1);
        Ok(())
    }

    #[test]
    fn test_observe_error_display_null_counter_violated() {
        let err = ObserveError::NullCounterViolated {
            name: "trust_block_violation_total",
            count: 3,
            invariant: "kein Fragment mit TrustClass::Data erscheint im Instruktionsblock",
        };
        assert_eq!(
            err.to_string(),
            "null counter 'trust_block_violation_total' is at 3 (expected zero); invariant: kein Fragment mit TrustClass::Data erscheint im Instruktionsblock"
        );
    }

    #[test]
    fn test_observe_error_null_counter_violated_source_is_none() {
        use std::error::Error as _;
        let err = ObserveError::NullCounterViolated {
            name: "x_total",
            count: 1,
            invariant: "invariant",
        };
        assert!(err.source().is_none());
    }
}
