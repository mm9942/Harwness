//! Fehlertyp für `harw-lens-embed`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`EmbedError`] für alle Fehler dieser Crate:
//! Katalog-Parsing, Routing-Fehlschläge und Dimensionsabweichungen eines
//! [`crate::embedder::Embedder`]. `Display`, die `Debug`-Ableitung und
//! `std::error::Error` werden nicht von Hand geschrieben, sondern von
//! `#[derive(harw_macros::HarwError)]` erzeugt (Muster:
//! `harw-lens-types/src/error.rs`). Kein `anyhow`, kein `thiserror`. Weil
//! der Enum-Name auf `Error` endet, erzeugt das Makro außerdem den
//! Typalias `EmbedResult<T>`.
//!
//! Exportierte Typen: [`EmbedError`], [`EmbedResult`].
//!
//! # Nebenläufigkeit
//! `EmbedError` ist reine Daten (`Clone`, `PartialEq`, `Eq`) und ohne
//! Einschränkung zwischen Threads teilbar.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{EmbedError, EmbeddingRole};
//!
//! let err = EmbedError::NoProfileForRole { role: EmbeddingRole::Prose };
//! assert!(err.to_string().contains("Prose"));
//! ```

use harw_macros::HarwError;

use crate::catalog::EmbeddingRole;

/// Fehler dieser Crate.
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zum Verstehen des
/// Fehlers ohne Quellcode-Lektüre nötig ist. Es entstehen ausschließlich
/// Validierungs- und Routing-Fehler über bereits vorhandene Werte -- diese
/// Crate tut kein I/O außer dem Parsen der eingebetteten `embeddings.toml`,
/// dessen Fehlermeldung als `String` in [`EmbedError::CatalogParse`] landet
/// (statt den fremden Fehlertyp zu wrappen, der weder `Clone` noch `Eq`
/// verspricht).
#[derive(Debug, Clone, PartialEq, Eq, HarwError)]
pub enum EmbedError {
    /// `embeddings.toml` (oder ein anderer TOML-Quelltext) ließ sich nicht
    /// als Katalog parsen -- ungültiges TOML oder ein unbekanntes Feld.
    ///
    /// # Arguments
    /// - `reason` (`String`): die Fehlermeldung des TOML-Parsers.
    #[msg("failed to parse embedding catalog: {reason}")]
    CatalogParse {
        /// Die Fehlermeldung des TOML-Parsers.
        reason: String,
    },

    /// Für die angefragte Rolle ist kein Modell im Katalog registriert.
    ///
    /// # Arguments
    /// - `role` (`EmbeddingRole`): die angefragte Rolle.
    #[msg("no embedding profile registered for role {role:?}")]
    NoProfileForRole {
        /// Die angefragte Rolle, für die kein Modell registriert ist.
        role: EmbeddingRole,
    },

    /// Für [`EmbeddingRole::Confidential`] existiert kein Modell mit
    /// [`harw_lens_types::Locality::Local`]. Fail-Closed: es wird nicht auf
    /// ein entferntes Profil ausgewichen.
    #[msg("confidential role requires a local embedding profile, but the catalog has none")]
    NoLocalProfileForConfidential,

    /// Ein [`crate::embedder::Embedder`] hat einen Vektor geliefert, dessen
    /// Länge von seiner eigenen `dimensions()`-Angabe abweicht.
    ///
    /// # Arguments
    /// - `expected` (`usize`): die von `dimensions()` behauptete Länge.
    /// - `actual` (`usize`): die tatsächliche Länge des gelieferten Vektors.
    #[msg("embedder produced a vector of length {actual}, but declared {expected} dimensions")]
    DimensionMismatch {
        /// Die von `Embedder::dimensions()` behauptete Länge.
        expected: usize,
        /// Die tatsächliche Länge des gelieferten Vektors.
        actual: usize,
    },

    /// Ein [`crate::remote::RemoteEmbedBackend`] (die vierte
    /// Einbettungsschicht, siehe [`crate::remote`]) konnte keine Vektoren
    /// liefern -- Transportfehler, Nicht-2xx-Status oder eine unbrauchbare
    /// Antwort. Trägt die Fehlermeldung als bereits formatierten `String`
    /// statt eines gewickelten Fremdfehlertyps: [`crate::http_backend::HttpEmbedBackend`]
    /// ist zwar seit Knoten AW7-06 konkret HTTP (`reqwest::Error`), ein
    /// künftiger zweiter `RemoteEmbedBackend` könnte aber ein anderes
    /// Transportformat (gRPC, …) mit einem eigenen Fehlertyp verwenden --
    /// diese Variante bleibt deshalb transportunabhängig.
    ///
    /// # Arguments
    /// - `reason` (`String`): die Fehlerbeschreibung des Backends.
    #[msg("remote embedding backend failed: {reason}")]
    RemoteBackendFailed {
        /// Die Fehlerbeschreibung des Backends.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embed_error_display_catalog_parse() {
        let err = EmbedError::CatalogParse {
            reason: "bad toml".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "failed to parse embedding catalog: bad toml"
        );
    }

    #[test]
    fn test_embed_error_display_no_profile_for_role() {
        let err = EmbedError::NoProfileForRole {
            role: EmbeddingRole::Prose,
        };
        assert_eq!(
            err.to_string(),
            "no embedding profile registered for role Prose"
        );
    }

    #[test]
    fn test_embed_error_display_no_local_profile_for_confidential() {
        let err = EmbedError::NoLocalProfileForConfidential;
        assert_eq!(
            err.to_string(),
            "confidential role requires a local embedding profile, but the catalog has none"
        );
    }

    #[test]
    fn test_embed_error_display_dimension_mismatch() {
        let err = EmbedError::DimensionMismatch {
            expected: 8,
            actual: 4,
        };
        assert_eq!(
            err.to_string(),
            "embedder produced a vector of length 4, but declared 8 dimensions"
        );
    }

    #[test]
    fn test_embed_error_display_remote_backend_failed() {
        let err = EmbedError::RemoteBackendFailed {
            reason: "timed out".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "remote embedding backend failed: timed out"
        );
    }
}
