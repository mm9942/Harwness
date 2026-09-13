//! Nutzungsbeschreibung eines Embedding-Modells: wie man es *benutzt*.
//!
//! # Verantwortungsbereich
//! Besitzt [`EmbeddingDescriptor`] sowie [`prepare_document`] und
//! [`prepare_query`] -- die einzigen Stellen, an denen Dokument- und
//! Abfrage-Präfixe angewendet werden. Vertauschte Präfixe verschlechtern die
//! Trefferqualität eines Embedding-Modells, ohne einen Fehler zu erzeugen;
//! deshalb kommen sie ausschließlich aus dem Deskriptor und werden nie am
//! Aufrufort von Hand geschrieben.
//!
//! # Nebenläufigkeit
//! Reine Daten (`Clone`, `PartialEq`); [`prepare_document`] und
//! [`prepare_query`] sind zustandslose, reine Funktionen.
//!
//! # Fehler
//! Erzeugt selbst keine Fehler.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{prepare_document, prepare_query, EmbeddingDescriptor};
//!
//! let descriptor = EmbeddingDescriptor {
//!     document_prefix: "passage: ".to_owned(),
//!     query_prefix: "query: ".to_owned(),
//!     normalize: true,
//! };
//! assert_ne!(
//!     prepare_document(&descriptor, "text"),
//!     prepare_query(&descriptor, "text"),
//! );
//! ```

use serde::{Deserialize, Serialize};

/// Wie ein Embedding-Modell *benutzt* wird.
///
/// # Description
/// Kommt aus der eingebetteten `embeddings.toml` (Tabelle
/// `model.descriptor`), getrennt von [`crate::spec::EmbeddingSpec`]:
/// Präfixe und Normalisierung sind eine Nutzungskonvention, keine
/// Eigenschaft des Modells selbst.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingDescriptor {
    /// Präfix, das jedem zu indizierenden Dokumenttext vorangestellt wird.
    pub document_prefix: String,
    /// Präfix, das jedem Abfragetext vorangestellt wird.
    pub query_prefix: String,
    /// Ob die vom Modell erzeugten Vektoren vor der Ablage normalisiert
    /// werden müssen.
    pub normalize: bool,
}

/// Bereitet einen Dokumenttext für das Einbetten vor.
///
/// # Description
/// Stellt `descriptor.document_prefix` voran. Das ist die einzige Stelle, an
/// der dieses Präfix angewendet wird -- kein Aufrufer schreibt es von Hand.
/// Vertauschte Präfixe (Dokument- statt Abfrage-Präfix oder umgekehrt)
/// verschlechtern die Trefferqualität, ohne einen Fehler zu erzeugen; diese
/// Funktion und [`prepare_query`] sind deshalb die einzigen Aufrufstellen.
///
/// # Arguments
/// - `descriptor` (`&EmbeddingDescriptor`): der Deskriptor des Zielmodells.
/// - `text` (`&str`): der rohe Dokumenttext.
///
/// # Returns
/// Der präfixierte Text als neuer `String`.
///
/// # Examples
/// ```rust
/// use harw_lens_embed::{prepare_document, EmbeddingDescriptor};
///
/// let descriptor = EmbeddingDescriptor {
///     document_prefix: "passage: ".to_owned(),
///     query_prefix: "query: ".to_owned(),
///     normalize: true,
/// };
/// assert_eq!(prepare_document(&descriptor, "hello"), "passage: hello");
/// ```
#[must_use]
pub fn prepare_document(descriptor: &EmbeddingDescriptor, text: &str) -> String {
    format!("{}{}", descriptor.document_prefix, text)
}

/// Bereitet einen Abfragetext für das Einbetten vor.
///
/// # Description
/// Stellt `descriptor.query_prefix` voran. Das ist die einzige Stelle, an
/// der dieses Präfix angewendet wird -- kein Aufrufer schreibt es von Hand.
///
/// # Arguments
/// - `descriptor` (`&EmbeddingDescriptor`): der Deskriptor des Zielmodells.
/// - `text` (`&str`): der rohe Abfragetext.
///
/// # Returns
/// Der präfixierte Text als neuer `String`.
///
/// # Examples
/// ```rust
/// use harw_lens_embed::{prepare_query, EmbeddingDescriptor};
///
/// let descriptor = EmbeddingDescriptor {
///     document_prefix: "passage: ".to_owned(),
///     query_prefix: "query: ".to_owned(),
///     normalize: true,
/// };
/// assert_eq!(prepare_query(&descriptor, "hello"), "query: hello");
/// ```
#[must_use]
pub fn prepare_query(descriptor: &EmbeddingDescriptor, text: &str) -> String {
    format!("{}{}", descriptor.query_prefix, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> EmbeddingDescriptor {
        EmbeddingDescriptor {
            document_prefix: "passage: ".to_owned(),
            query_prefix: "query: ".to_owned(),
            normalize: true,
        }
    }

    // Diese beiden Tests prüfen exakt, welches Präfix jede Funktion
    // anwendet -- nicht nur, dass die Ergebnisse verschieden sind. Eine
    // vertauschte Implementierung (prepare_document nutzt query_prefix und
    // umgekehrt) würde beide Tests zum Scheitern bringen, weil die
    // erwarteten Strings an das jeweils falsche Präfix gebunden sind.
    #[test]
    fn test_prepare_document_applies_document_prefix() {
        assert_eq!(prepare_document(&descriptor(), "hello"), "passage: hello");
    }

    #[test]
    fn test_prepare_query_applies_query_prefix() {
        assert_eq!(prepare_query(&descriptor(), "hello"), "query: hello");
    }

    /// Der Test, der die Präfixsemantik trägt: bei unterschiedlichen
    /// Präfixen im Deskriptor müssen `prepare_document` und `prepare_query`
    /// unterschiedliche Ergebnisse liefern.
    #[test]
    fn test_prepare_document_and_prepare_query_differ_for_distinct_prefixes() {
        let d = descriptor();
        assert_ne!(
            prepare_document(&d, "same text"),
            prepare_query(&d, "same text")
        );
    }

    #[test]
    fn test_prepare_document_and_prepare_query_agree_when_prefixes_match() {
        let d = EmbeddingDescriptor {
            document_prefix: "same: ".to_owned(),
            query_prefix: "same: ".to_owned(),
            normalize: false,
        };
        assert_eq!(prepare_document(&d, "x"), prepare_query(&d, "x"));
    }
}
