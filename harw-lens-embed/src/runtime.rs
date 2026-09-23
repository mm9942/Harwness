//! Laufzeitprofil: was zur Laufzeit *gilt*.
//!
//! # Verantwortungsbereich
//! Besitzt [`RuntimeProfile`] -- Backend-Kennung, [`Locality`] und
//! Stapelgröße, mit denen ein Modell tatsächlich betrieben wird. Getrennt
//! von [`crate::spec::EmbeddingSpec`]: die Spezifikation beschreibt, was ein
//! Modell laut Definition ist; dieses Profil beschreibt, wie es aktuell
//! bereitgestellt ist. Ein Backend, das eine andere Dimensionszahl liefert
//! als die Spezifikation verspricht, ist nur als Fehler sichtbar, wenn beide
//! Werte in getrennten Typen liegen -- [`crate::embedder::DimensionCheckedEmbedder`]
//! prüft genau das beim tatsächlichen Aufruf.
//!
//! # Nebenläufigkeit
//! Reine Daten (`Clone`, `PartialEq`), ohne Einschränkung zwischen Threads
//! teilbar.
//!
//! # Fehler
//! Erzeugt selbst keine Fehler.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::RuntimeProfile;
//! use harw_lens_types::Locality;
//!
//! let profile = RuntimeProfile {
//!     backend: "onnx-local".to_owned(),
//!     locality: Locality::Local,
//!     batch_size: 32,
//! };
//! assert_eq!(profile.locality, Locality::Local);
//! ```

use harw_lens_types::Locality;
use serde::{Deserialize, Serialize};

/// Was zur Laufzeit *gilt*.
///
/// # Description
/// Kommt aus der eingebetteten `embeddings.toml` (Tabelle `model.runtime`),
/// ein Eintrag je Modell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfile {
    /// Kennung des Backends, über das dieses Modell tatsächlich läuft.
    pub backend: String,
    /// Wo dieses Backend rechnet.
    pub locality: Locality,
    /// Anzahl der Texte, die in einem Aufruf gemeinsam eingebettet werden.
    pub batch_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_runtime_profile_parses_from_toml() -> TestResult {
        let toml_src = r#"
            backend = "onnx-local"
            locality = "local"
            batch_size = 32
        "#;
        let profile: RuntimeProfile = toml::from_str(toml_src).map_err(ctx("parses"))?;
        assert_eq!(profile.backend, "onnx-local");
        assert_eq!(profile.locality, Locality::Local);
        assert_eq!(profile.batch_size, 32);
        Ok(())
    }

    #[test]
    fn test_runtime_profile_rejects_unknown_field() {
        let toml_src = r#"
            backend = "onnx-local"
            locality = "local"
            batch_size = 32
            extra = true
        "#;
        assert!(toml::from_str::<RuntimeProfile>(toml_src).is_err());
    }
}
