//! Fehlertyp für `harw-observe-prom`.
//!
//! # Verantwortungsbereich
//! Trägt [`PromError`], den einen Fehlertyp dieser Crate (Vertrag §H.1,
//! `docs/design/build-history.md`). Die einzige fehlbare Operation dieser
//! Crate ist [`crate::PromEndpoint::bind`]: Socket-Bindung (TCP-Loopback
//! oder Unix-Socket) und die Typ-Zwang-Tiefenverteidigung gegen eine
//! Nicht-Loopback-Adresse (siehe Moduldoc von `crate::endpoint`).
//! [`crate::PromSink::record`] und `::flush` geben `()` zurück (Vertrag A.3)
//! und können deshalb keinen `PromError` erzeugen.
//!
//! # Nebenläufigkeit
//! `PromError` trägt nur `String`- und `std::io::Error`-Felder, ist
//! `Send + Sync` und ohne Interior Mutability — beliebig zwischen Threads
//! teilbar.
//!
//! # Fehler
//! `Display`, `Debug` (zusätzliche Ableitung), `std::error::Error` und der
//! `PromResult<T>`-Alias entstehen über `#[derive(harw_macros::HarwError)]`.
//! Kein `anyhow`, kein `thiserror`.
//!
//! # Examples
//! ```
//! use harw_observe_prom::PromError;
//!
//! let source = std::io::Error::new(std::io::ErrorKind::AddrInUse, "in use");
//! let err = PromError::Bind {
//!     address: "127.0.0.1:9327".to_owned(),
//!     source,
//! };
//! assert!(err.to_string().contains("127.0.0.1:9327"));
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate (Vertrag Kopfteil „Fehler").
///
/// # Description
/// Beide Varianten tragen die betroffene Adresse als `String` (nicht
/// `SocketAddr`/`PathBuf`, die keine für `#[msg(...)]` nutzbare `Display`-
/// Form ohne Umweg hätten — Muster: `harw-observe-file/src/error.rs`).
/// [`PromError::NonLoopbackBind`] ist über die öffentliche
/// [`crate::PromEndpoint::bind`]-Signatur aktuell **nicht erreichbar**:
/// [`crate::BindAddr`] hat keine Variante, die eine externe Adresse
/// ausdrückt (Typ-Zwang, siehe Moduldoc von `crate::endpoint`). Die Variante
/// bleibt als Tiefenverteidigung bestehen und wird direkt über die private
/// Prüffunktion getestet, die sie erzeugt.
#[derive(Debug, HarwError)]
pub enum PromError {
    /// Der Endpunkt konnte nicht gebunden werden — TCP-Adresse belegt,
    /// Unix-Socket-Pfad nicht anlegbar, oder der Nichtblockierend-Modus
    /// ließ sich nicht setzen.
    ///
    /// # Arguments
    /// - `address` (`String`): die betroffene TCP-Adresse oder der
    ///   betroffene Unix-Socket-Pfad.
    /// - `source` (`std::io::Error`): die zugrunde liegende I/O-Ursache.
    #[msg("failed to bind Prometheus endpoint at '{address}': {source}")]
    Bind {
        /// Die betroffene TCP-Adresse oder der betroffene Unix-Socket-Pfad.
        address: String,
        /// Die zugrunde liegende I/O-Ursache.
        source: std::io::Error,
    },

    /// Tiefenverteidigung gegen eine Nicht-Loopback-Bindeadresse. Über die
    /// öffentliche API aktuell unerreichbar (siehe `# Description` oben),
    /// weil [`crate::BindAddr`] eine solche Adresse gar nicht ausdrücken
    /// kann; direkt gegen die interne Prüffunktion getestet.
    ///
    /// # Arguments
    /// - `address` (`String`): die abgelehnte, nicht-loopback Adresse.
    #[msg("refusing to bind Prometheus endpoint to non-loopback address '{address}'")]
    NonLoopbackBind {
        /// Die abgelehnte, nicht-loopback Adresse.
        address: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn io_error() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::AddrInUse, "already in use")
    }

    #[test]
    fn test_prom_error_bind_display_contains_address_and_source() {
        let err = PromError::Bind {
            address: "127.0.0.1:9327".to_owned(),
            source: io_error(),
        };
        let message = err.to_string();
        assert!(message.contains("127.0.0.1:9327"));
        assert!(message.contains("already in use"));
    }

    #[test]
    fn test_prom_error_non_loopback_bind_display_contains_address() {
        let err = PromError::NonLoopbackBind {
            address: "203.0.113.5:9327".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "refusing to bind Prometheus endpoint to non-loopback address '203.0.113.5:9327'"
        );
    }

    #[test]
    fn test_prom_error_source_is_none_for_named_field_variants() {
        use std::error::Error as _;
        let err = PromError::Bind {
            address: "x".to_owned(),
            source: io_error(),
        };
        // `#[from]` verlangt eine Tupel-Variante mit genau einem Feld
        // (harw-macros/src/error.rs); benannte Felder mit zusätzlichem
        // Kontext (hier `address`) verketten `source()` deshalb nicht.
        assert!(err.source().is_none());
    }

    #[test]
    fn test_prom_result_alias_exists() -> TestResult {
        fn make() -> PromResult<u8> {
            Ok(1)
        }
        assert_eq!(make().map_err(ctx("prom result alias"))?, 1);
        Ok(())
    }
}
