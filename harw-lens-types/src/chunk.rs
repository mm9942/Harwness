//! Herkunfts- und Chunk-Vokabular für `harw-lens-types`.
//!
//! # Verantwortungsbereich
//! Besitzt [`SourceRef`] (woher ein Chunk stammt), [`ByteSpan`] (welcher
//! Byte-Bereich in der Quelle), [`ChunkDigest`] (das Newtype über
//! [`harw_types::ContentDigest`] für Chunk-Inhalte) und [`Chunk`] selbst.
//! Dieses Modul tut kein I/O: das Einlesen von Quellen und das Zerlegen von
//! Text in Chunks lebt in `harw-lens-chunk` (AW0-... nachgelagert), nicht
//! hier. `harw-lens-types` liefert nur das Vokabular, in dem jene Crate
//! spricht.
//!
//! # Nebenläufigkeit
//! Alle Typen dieses Moduls sind reine Daten (`Clone`, `PartialEq`) ohne
//! interne Veränderlichkeit und ohne Einschränkung zwischen Threads teilbar.
//! [`ByteSpan`] und [`ChunkDigest`] sind zusätzlich `Copy`.
//!
//! # Fehler
//! [`crate::LensTypesError::InvalidSpan`] und
//! [`crate::LensTypesError::SpanOutOfBounds`] entstehen ausschließlich hier,
//! über [`ByteSpan::new`] und [`ByteSpan::validate`].
//!
//! Contract-Master Abschnitt C (AW0-08, `docs/aw-contract-master.md`).

use serde::{Deserialize, Serialize};

use crate::error::LensTypesError;

/// Woher ein Chunk stammt. Geschlossen und ohne Interpretation: dieser Typ
/// sagt, wo etwas herkommt, nie was es bedeutet.
///
/// # Description
/// Serialisiert extern getaggt mit kebab-case-Variantennamen (`file`,
/// `artifact`, `plan-node`, `diary`), damit Bestandsdateien lesbar bleiben,
/// falls eine Variante hinzukommt. `deny_unknown_fields` lehnt jedes
/// zusätzliche Feld innerhalb einer Variante ab.
///
/// # Examples
/// ```rust
/// use harw_lens_types::SourceRef;
///
/// let source = SourceRef::File { path: "src/lib.rs".to_owned() };
/// let json = serde_json::to_string(&source).unwrap();
/// assert!(json.contains("\"file\""));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum SourceRef {
    /// Eine Datei auf der Platte, referenziert über ihren Pfad.
    File {
        /// Pfad der Datei, wie er im Chunk-Store abgelegt wurde.
        path: String,
    },
    /// Ein Artefakt aus einer nicht-dateibasierten Quelle.
    Artifact {
        /// Kennung des Artefakts.
        id: String,
    },
    /// Ein Knoten in einem Plan.
    PlanNode {
        /// Kennung des Plans.
        plan: String,
        /// Kennung des Knotens innerhalb des Plans.
        node: String,
    },
    /// Ein Eintrag in einem Tagebuch/Diary.
    Diary {
        /// Kennung des Eintrags.
        entry: String,
    },
}

/// Ein Byte-Bereich in der Quelle.
///
/// # Description
/// Die Felder sind absichtlich öffentlich: `ByteSpan` selbst erzwingt seine
/// Invariante (`start <= end`) nur über [`ByteSpan::new`]; wer die Felder
/// direkt setzt (etwa beim Deserialisieren aus einer Bestandsdatei), erhält
/// keine erneute Prüfung geschenkt. [`ByteSpan::validate`] prüft zusätzlich
/// gegen die tatsächliche Textlänge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteSpan {
    /// Anfang des Bereichs, inklusive, in Bytes.
    pub start: usize,
    /// Ende des Bereichs, exklusiv, in Bytes.
    pub end: usize,
}

impl ByteSpan {
    /// Baut einen Byte-Bereich aus `start` und `end`.
    ///
    /// # Description
    /// Prüft ausschließlich `start <= end`. Ob der Bereich innerhalb eines
    /// gegebenen Textes liegt, prüft [`ByteSpan::validate`] separat, weil ein
    /// `ByteSpan` ohne zugehörigen Text konstruierbar bleiben muss (etwa beim
    /// Deserialisieren aus einem gespeicherten [`Chunk`]).
    ///
    /// # Arguments
    /// - `start` (`usize`): Anfang des Bereichs, inklusive.
    /// - `end` (`usize`): Ende des Bereichs, exklusiv.
    ///
    /// # Returns
    /// `Ok(ByteSpan)` bei `start <= end`.
    ///
    /// # Errors
    /// - [`LensTypesError::InvalidSpan`]: wenn `start > end`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_types::{ByteSpan, LensTypesError};
    ///
    /// assert_eq!(ByteSpan::new(0, 4), Ok(ByteSpan { start: 0, end: 4 }));
    /// assert_eq!(
    ///     ByteSpan::new(4, 0),
    ///     Err(LensTypesError::InvalidSpan { start: 4, end: 0 })
    /// );
    /// ```
    pub fn new(start: usize, end: usize) -> Result<Self, LensTypesError> {
        if start > end {
            return Err(LensTypesError::InvalidSpan { start, end });
        }
        Ok(Self { start, end })
    }

    /// Prüft, ob dieser Bereich innerhalb von `text` liegt.
    ///
    /// # Arguments
    /// - `text` (`&str`): der Text, gegen dessen Byte-Länge geprüft wird.
    ///
    /// # Returns
    /// `Ok(())`, wenn `self.end` die Byte-Länge von `text` nicht überschreitet.
    ///
    /// # Errors
    /// - [`LensTypesError::SpanOutOfBounds`]: wenn `self.end` über die
    ///   Byte-Länge von `text` hinausreicht.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_types::ByteSpan;
    ///
    /// let span = ByteSpan::new(0, 5).unwrap();
    /// assert!(span.validate("hello").is_ok());
    /// assert!(span.validate("hi").is_err());
    /// ```
    pub fn validate(&self, text: &str) -> Result<(), LensTypesError> {
        let len = text.len();
        if self.end > len {
            return Err(LensTypesError::SpanOutOfBounds {
                start: self.start,
                end: self.end,
                len,
            });
        }
        Ok(())
    }
}

/// Der Digest eines Chunks.
///
/// # Description
/// Ein Newtype über [`harw_types::ContentDigest`] statt eines Alias: ein
/// Chunk-Digest und ein Nachweis-Digest sind beide 32 Bytes und dürfen
/// trotzdem nie verwechselt werden. Das Hashen selbst lebt an genau einer
/// Stelle ([`harw_types::ContentDigest::of`]). Serde delegiert transparent an
/// die innere Serialisierung des gewickelten Wertes (Hex-Zeichenkette).
///
/// # Examples
/// ```rust
/// use harw_lens_types::ChunkDigest;
/// use harw_types::ContentDigest;
///
/// let digest = ChunkDigest(ContentDigest::of(b"chunk text"));
/// let json = serde_json::to_string(&digest).unwrap();
/// let round_tripped: ChunkDigest = serde_json::from_str(&json).unwrap();
/// assert_eq!(round_tripped, digest);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChunkDigest(pub harw_types::ContentDigest);

/// Ein Stück Text mit Herkunft.
///
/// # Description
/// Trägt alles, was ein Konsument braucht, um den Text zu zitieren
/// ([`SourceRef`], [`ByteSpan`]) und um ihn wiederzuerkennen ([`ChunkDigest`]).
/// Die Felder sind öffentlich; die Invariante „`span` liegt innerhalb von
/// `text`" wird nicht vom Typ selbst erzwungen, sondern vom Aufrufer über
/// [`ByteSpan::validate`] geprüft, bevor ein `Chunk` gebaut wird — analog zu
/// [`ByteSpan`] selbst.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    /// Digest über `text`.
    pub digest: ChunkDigest,
    /// Woher `text` stammt.
    pub source: SourceRef,
    /// Byte-Bereich von `text` innerhalb der Quelle.
    pub span: ByteSpan,
    /// Der Chunk-Inhalt selbst.
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_types::ContentDigest;

    #[test]
    fn test_byte_span_new_accepts_start_less_than_end() {
        assert_eq!(ByteSpan::new(0, 4), Ok(ByteSpan { start: 0, end: 4 }));
    }

    #[test]
    fn test_byte_span_new_accepts_start_equal_end() {
        assert_eq!(ByteSpan::new(3, 3), Ok(ByteSpan { start: 3, end: 3 }));
    }

    #[test]
    fn test_byte_span_new_rejects_start_greater_than_end() {
        assert_eq!(
            ByteSpan::new(4, 0),
            Err(LensTypesError::InvalidSpan { start: 4, end: 0 })
        );
    }

    #[test]
    fn test_byte_span_validate_accepts_span_within_text() -> TestResult {
        let span = ByteSpan::new(0, 5).map_err(ctx("valid span"))?;
        assert_eq!(span.validate("hello"), Ok(()));
        Ok(())
    }

    #[test]
    fn test_byte_span_validate_rejects_span_past_text_end() -> TestResult {
        let span = ByteSpan::new(0, 10).map_err(ctx("valid span"))?;
        assert_eq!(
            span.validate("hi"),
            Err(LensTypesError::SpanOutOfBounds {
                start: 0,
                end: 10,
                len: 2,
            })
        );
        Ok(())
    }

    #[test]
    fn test_source_ref_serializes_kebab_case_variant_tags() -> TestResult {
        let file = SourceRef::File {
            path: "src/lib.rs".to_owned(),
        };
        let artifact = SourceRef::Artifact {
            id: "a-1".to_owned(),
        };
        let plan_node = SourceRef::PlanNode {
            plan: "p-1".to_owned(),
            node: "n-1".to_owned(),
        };
        let diary = SourceRef::Diary {
            entry: "e-1".to_owned(),
        };

        assert!(
            serde_json::to_string(&file)
                .map_err(ctx("serializes"))?
                .contains("\"file\"")
        );
        assert!(
            serde_json::to_string(&artifact)
                .map_err(ctx("serializes"))?
                .contains("\"artifact\"")
        );
        assert!(
            serde_json::to_string(&plan_node)
                .map_err(ctx("serializes"))?
                .contains("\"plan-node\"")
        );
        assert!(
            serde_json::to_string(&diary)
                .map_err(ctx("serializes"))?
                .contains("\"diary\"")
        );
        Ok(())
    }

    #[test]
    fn test_source_ref_roundtrips_through_json() -> TestResult {
        let source = SourceRef::PlanNode {
            plan: "p-1".to_owned(),
            node: "n-1".to_owned(),
        };
        let json = serde_json::to_string(&source).map_err(ctx("serializes"))?;
        let round_tripped: SourceRef = serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, source);
        Ok(())
    }

    #[test]
    fn test_source_ref_rejects_unknown_field() {
        let json = r#"{"file":{"path":"x","extra":true}}"#;
        assert!(serde_json::from_str::<SourceRef>(json).is_err());
    }

    #[test]
    fn test_chunk_digest_serializes_transparently_as_hex_string() -> TestResult {
        let digest = ChunkDigest(ContentDigest::of(b"chunk text"));
        let json = serde_json::to_string(&digest).map_err(ctx("serializes"))?;
        assert!(json.starts_with('"') && json.ends_with('"'));
        Ok(())
    }

    #[test]
    fn test_chunk_digest_roundtrips_through_json() -> TestResult {
        let digest = ChunkDigest(ContentDigest::of(b"chunk text"));
        let json = serde_json::to_string(&digest).map_err(ctx("serializes"))?;
        let round_tripped: ChunkDigest =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, digest);
        Ok(())
    }

    #[test]
    fn test_chunk_digest_differs_for_different_content() {
        let a = ChunkDigest(ContentDigest::of(b"one"));
        let b = ChunkDigest(ContentDigest::of(b"two"));
        assert_ne!(a, b);
    }

    #[test]
    fn test_chunk_roundtrips_through_json() -> TestResult {
        let chunk = Chunk {
            digest: ChunkDigest(ContentDigest::of(b"hello")),
            source: SourceRef::File {
                path: "a.txt".to_owned(),
            },
            span: ByteSpan::new(0, 5).map_err(ctx("valid span"))?,
            text: "hello".to_owned(),
        };
        let json = serde_json::to_string(&chunk).map_err(ctx("serializes"))?;
        let round_tripped: Chunk = serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, chunk);
        Ok(())
    }

    #[test]
    fn test_chunk_rejects_unknown_field() {
        let json = r#"{
            "digest": "00000000000000000000000000000000000000000000000000000000000000",
            "source": {"file": {"path": "a.txt"}},
            "span": {"start": 0, "end": 1},
            "text": "a",
            "extra": true
        }"#;
        assert!(serde_json::from_str::<Chunk>(json).is_err());
    }
}
