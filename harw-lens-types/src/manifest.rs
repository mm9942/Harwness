//! Index-Manifest-Vokabular für `harw-lens-types`.
//!
//! # Verantwortungsbereich
//! Besitzt [`Locality`], [`Metric`] und [`IndexManifest`]: was ein
//! Vektorindex über sich selbst behauptet, und wie zwei Manifeste auf
//! Kompatibilität geprüft werden. Dieses Modul baut oder liest keinen
//! tatsächlichen Index — das lebt in `harw-lens-index` (nachgelagert).
//!
//! # Nebenläufigkeit
//! [`IndexManifest`] ist reine Daten (`Clone`, `PartialEq`) ohne interne
//! Veränderlichkeit und ohne Einschränkung zwischen Threads teilbar.
//!
//! # Fehler
//! [`crate::LensTypesError::ManifestMismatch`] entsteht ausschließlich über
//! [`IndexManifest::compatible_with`].
//!
//! Contract-Master Abschnitt C (AW0-08, `docs/aw-contract-master.md`).

use serde::{Deserialize, Serialize};

use crate::error::LensTypesError;

/// Wo ein Embedding berechnet werden darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Locality {
    /// Embedding entsteht auf demselben Host wie der Index.
    Local,
    /// Embedding entsteht über einen entfernten Dienst.
    Remote,
}

/// Abstandsmaß eines Index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Metric {
    /// Kosinus-Ähnlichkeit.
    Cosine,
    /// Skalarprodukt.
    DotProduct,
    /// Euklidischer Abstand.
    Euclidean,
}

/// Was ein Index über sich selbst behauptet.
///
/// # Description
/// Eine Abfrage gegen ein abweichendes `model` oder eine abweichende
/// `chunker_version` wird über [`IndexManifest::compatible_with`] abgelehnt,
/// nie stillschweigend beantwortet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexManifest {
    /// Name/Kennung des Embedding-Modells, mit dem der Index gebaut wurde.
    pub model: String,
    /// Wo das Embedding berechnet wird.
    pub locality: Locality,
    /// Version des Chunkers, mit dem der Index gebaut wurde.
    pub chunker_version: u32,
    /// Sichtbarkeitsklasse des Index (freier Text, von `harw-lens-index`
    /// interpretiert).
    pub visibility: String,
    /// Abstandsmaß des Index.
    pub metric: Metric,
    /// Digest über die Menge der indizierten Quellen.
    pub source_set_digest: harw_types::ContentDigest,
}

impl IndexManifest {
    /// Prüft, ob eine Abfrage gegen `self` mit dem Manifest `other` beantwortet
    /// werden darf.
    ///
    /// # Description
    /// Vergleicht ausschließlich die beiden Felder, gegen die eine
    /// unbemerkte Abweichung ein falsches Ergebnis liefern würde: `model`
    /// und `chunker_version`. Ein unterschiedliches `metric`, `locality`
    /// oder `visibility` entscheidet diese Prüfung bewusst nicht — dafür
    /// existiert (falls nötig) eine gesonderte Prüfung in `harw-lens-index`.
    ///
    /// # Arguments
    /// - `other` (`&IndexManifest`): das Manifest, gegen das geprüft wird.
    ///
    /// # Returns
    /// `Ok(())`, wenn `model` und `chunker_version` in beiden Manifesten
    /// übereinstimmen.
    ///
    /// # Errors
    /// - [`LensTypesError::ManifestMismatch`]: mit `field = "model"`, wenn
    ///   die Modelle abweichen, sonst mit `field = "chunker_version"`, wenn
    ///   die Chunker-Versionen abweichen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_types::{IndexManifest, Locality, Metric, LensTypesError};
    /// use harw_types::ContentDigest;
    ///
    /// let manifest = IndexManifest {
    ///     model: "text-embed-3".to_owned(),
    ///     locality: Locality::Local,
    ///     chunker_version: 1,
    ///     visibility: "workspace".to_owned(),
    ///     metric: Metric::Cosine,
    ///     source_set_digest: ContentDigest::of(b"sources"),
    /// };
    /// let mut other = manifest.clone();
    /// other.model = "text-embed-4".to_owned();
    ///
    /// assert_eq!(
    ///     manifest.compatible_with(&other),
    ///     Err(LensTypesError::ManifestMismatch { field: "model" })
    /// );
    /// ```
    pub fn compatible_with(&self, other: &Self) -> Result<(), LensTypesError> {
        if self.model != other.model {
            return Err(LensTypesError::ManifestMismatch { field: "model" });
        }
        if self.chunker_version != other.chunker_version {
            return Err(LensTypesError::ManifestMismatch {
                field: "chunker_version",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::ContentDigest;

    fn sample_manifest() -> IndexManifest {
        IndexManifest {
            model: "text-embed-3".to_owned(),
            locality: Locality::Local,
            chunker_version: 1,
            visibility: "workspace".to_owned(),
            metric: Metric::Cosine,
            source_set_digest: ContentDigest::of(b"sources"),
        }
    }

    #[test]
    fn test_compatible_with_accepts_identical_model_and_chunker_version() {
        let a = sample_manifest();
        let mut b = sample_manifest();
        b.visibility = "private".to_owned();
        b.metric = Metric::Euclidean;

        assert_eq!(a.compatible_with(&b), Ok(()));
    }

    #[test]
    fn test_compatible_with_rejects_model_mismatch() {
        let a = sample_manifest();
        let mut b = sample_manifest();
        b.model = "text-embed-4".to_owned();

        assert_eq!(
            a.compatible_with(&b),
            Err(LensTypesError::ManifestMismatch { field: "model" })
        );
    }

    #[test]
    fn test_compatible_with_rejects_chunker_version_mismatch() {
        let a = sample_manifest();
        let mut b = sample_manifest();
        b.chunker_version = 2;

        assert_eq!(
            a.compatible_with(&b),
            Err(LensTypesError::ManifestMismatch {
                field: "chunker_version"
            })
        );
    }

    #[test]
    fn test_compatible_with_checks_model_before_chunker_version() {
        let a = sample_manifest();
        let mut b = sample_manifest();
        b.model = "text-embed-4".to_owned();
        b.chunker_version = 2;

        assert_eq!(
            a.compatible_with(&b),
            Err(LensTypesError::ManifestMismatch { field: "model" })
        );
    }

    #[test]
    fn test_index_manifest_roundtrips_through_json() {
        let manifest = sample_manifest();
        let json = serde_json::to_string(&manifest).expect("serializes");
        let round_tripped: IndexManifest = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(round_tripped, manifest);
    }

    #[test]
    fn test_index_manifest_rejects_unknown_field() {
        let json = r#"{
            "model": "m",
            "locality": "local",
            "chunker_version": 1,
            "visibility": "workspace",
            "metric": "cosine",
            "source_set_digest": "00000000000000000000000000000000000000000000000000000000000000",
            "extra": true
        }"#;
        assert!(serde_json::from_str::<IndexManifest>(json).is_err());
    }

    #[test]
    fn test_locality_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Locality::Local).unwrap(),
            "\"local\""
        );
        assert_eq!(
            serde_json::to_string(&Locality::Remote).unwrap(),
            "\"remote\""
        );
    }

    #[test]
    fn test_metric_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Metric::DotProduct).unwrap(),
            "\"dot-product\""
        );
    }
}
