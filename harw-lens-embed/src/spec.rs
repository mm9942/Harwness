//! Modell-Spezifikation: was ein Embedding-Modell *ist*.
//!
//! # Verantwortungsbereich
//! Besitzt [`EmbeddingSpec`] -- Name, Dimensionszahl, Metrik und maximale
//! Eingabelänge eines Embedding-Modells. Unveränderlich und aus der
//! eingebetteten `embeddings.toml` gelesen (siehe [`crate::catalog`]).
//!
//! Getrennt von [`crate::runtime::RuntimeProfile`], weil ein Modell, das laut
//! Spezifikation z. B. 768 Dimensionen hat, aber über ein Backend läuft, das
//! tatsächlich nur 512 liefert, ein Fehler ist -- sichtbar nur, wenn
//! Spezifikation und Laufzeitwert getrennte Felder in getrennten Typen sind.
//! Ein gemeinsames Struct ließe eine Zuweisung die andere überschreiben, und
//! die Abweichung verschwände spurlos.
//!
//! # Nebenläufigkeit
//! Reine Daten (`Clone`, `PartialEq`), ohne Einschränkung zwischen Threads
//! teilbar.
//!
//! # Fehler
//! Erzeugt selbst keine Fehler; Deserialisierungsfehler entstehen beim
//! Parsen der Katalogdatei in [`crate::catalog`].
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::EmbeddingSpec;
//! use harw_lens_types::Metric;
//!
//! let spec = EmbeddingSpec {
//!     name: "local-minilm-l6-v2".to_owned(),
//!     dimensions: 384,
//!     metric: Metric::Cosine,
//!     max_input_chars: 2048,
//! };
//! assert_eq!(spec.dimensions, 384);
//! ```

use harw_lens_types::Metric;
use serde::{Deserialize, Serialize};

/// Was ein Embedding-Modell *ist*.
///
/// # Description
/// Unveränderliche Eigenschaften eines Modells, unabhängig davon, wo oder
/// wie es zur Laufzeit betrieben wird. Kommt aus der eingebetteten
/// `embeddings.toml` (Tabelle `model.spec`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingSpec {
    /// Name/Kennung des Modells.
    pub name: String,
    /// Anzahl der Dimensionen eines von diesem Modell erzeugten Vektors.
    pub dimensions: usize,
    /// Abstandsmaß, mit dem Vektoren dieses Modells verglichen werden.
    pub metric: Metric,
    /// Maximale Eingabelänge in Zeichen, die dieses Modell akzeptiert.
    pub max_input_chars: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_spec_parses_from_toml() {
        let toml_src = r#"
            name = "local-minilm-l6-v2"
            dimensions = 384
            metric = "cosine"
            max_input_chars = 2048
        "#;
        let spec: EmbeddingSpec = toml::from_str(toml_src).expect("parses");
        assert_eq!(spec.name, "local-minilm-l6-v2");
        assert_eq!(spec.dimensions, 384);
        assert_eq!(spec.metric, Metric::Cosine);
        assert_eq!(spec.max_input_chars, 2048);
    }

    #[test]
    fn test_embedding_spec_rejects_unknown_field() {
        let toml_src = r#"
            name = "m"
            dimensions = 384
            metric = "cosine"
            max_input_chars = 2048
            extra = true
        "#;
        assert!(toml::from_str::<EmbeddingSpec>(toml_src).is_err());
    }
}
