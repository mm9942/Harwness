//! Einbettungs-Backend: `Embedder`-Trait und ein deterministischer
//! Test-Embedder.
//!
//! # Verantwortungsbereich
//! Besitzt [`Embedder`], [`DeterministicEmbedder`] und
//! [`DimensionCheckedEmbedder`]. Kein HTTP-Client, keine ML-Abhängigkeit --
//! dieses Modul selbst bleibt reines Trait plus Testdoppel. Seit Knoten
//! **AW7-06** trägt die vierte Einbettungsschicht
//! ([`crate::remote::RemoteEmbedder`]) mit [`crate::http_backend::HttpEmbedBackend`]
//! einen echten, eigenständigen HTTP-Transport -- siehe dessen Moduldoku für
//! die Begründung, warum dieser Transport in dieser Crate selbst liegt und
//! nicht über `harw-provider`/`harw-provider-http` läuft.
//!
//! Ein stillschweigend falsch dimensionierter Vektor macht einen ganzen
//! Index unbrauchbar, und der Fehler zeigt sich erst bei der Abfrage.
//! [`DimensionCheckedEmbedder`] macht diese Prüfung zur Aufrufzeit, nicht
//! erst bei der späteren Abfrage.
//!
//! # `Embedder::locality` -- die Selbstauskunft, auf der der Nullzähler beruht
//! [`Embedder::locality`] ist eine Default-Methode, die **fehlschließt** auf
//! [`Locality::Remote`]: ein `Embedder`, der nicht ausdrücklich erklärt,
//! lokal zu rechnen, gilt als potenziell netzverlassend. Der Grund für diese
//! Richtung (nicht `Local` als Default) ist derselbe wie bei
//! [`crate::catalog::route`]s Fail-Closed-Verhalten für
//! [`crate::catalog::EmbeddingRole::Confidential`]: eine falsch-negative
//! Annahme (ein tatsächlich entfernter Embedder gilt als lokal) wäre ein
//! stiller Datenabfluss; eine falsch-positive Annahme (ein tatsächlich
//! lokaler Embedder gilt als entfernt, weil er `locality()` vergessen hat zu
//! überschreiben) ist nur ein zu strenger Fehler, kein Sicherheitsproblem.
//! [`DeterministicEmbedder`] überschreibt die Methode ausdrücklich auf
//! [`Locality::Local`] -- nicht weil der Default das verlangt, sondern damit
//! ein Leser der Implementierung nicht raten muss. `harw-lens-source::build_index`
//! liest genau dieses Signal, um zu entscheiden, ob ein Embedder für einen
//! `operator-only`-Sichtbarkeits-Bucket zulässig ist (siehe dessen
//! Moduldoku und den Nullzähler `lens_remote_embed_on_operator_only`).
//!
//! # Nebenläufigkeit
//! [`Embedder`] ist `Send + Sync`: Implementierungen werden hinter
//! `&dyn Embedder` geteilt und aus mehreren Threads aufgerufen. `embed`
//! nimmt `&self` und darf keinen veränderlichen Zustand über den Aufruf
//! hinaus führen.
//!
//! # Fehler
//! [`crate::error::EmbedError::DimensionMismatch`], wenn ein `Embedder`
//! einen Vektor liefert, dessen Länge von `dimensions()` abweicht.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{DeterministicEmbedder, Embedder};
//!
//! let embedder = DeterministicEmbedder::new(16);
//! let vectors = embedder
//!     .embed(&["hello".to_owned(), "world".to_owned()])
//!     .expect("deterministic embedder never fails");
//! assert_eq!(vectors.len(), 2);
//! assert_eq!(vectors[0].len(), 16);
//! assert_eq!(embedder.locality(), harw_lens_types::Locality::Local);
//! ```

use harw_lens_types::Locality;

use crate::error::EmbedError;

/// Erzeugt Vektoren aus Texten.
///
/// # Description
/// Der eine Zugang zu einem Embedding-Backend, unabhängig davon, ob es
/// lokal oder entfernt rechnet. Implementierungen liefern für jeden
/// Eingabetext genau einen Vektor der Länge `dimensions()`.
///
/// # Concurrency
/// `Send + Sync`: Implementierungen werden hinter `&dyn Embedder` geteilt
/// und aus mehreren Threads aufgerufen.
pub trait Embedder: Send + Sync {
    /// Bettet eine Liste von Texten ein.
    ///
    /// # Arguments
    /// - `texts` (`&[String]`): die bereits präfixierten Texte (siehe
    ///   [`crate::descriptor::prepare_document`]/[`crate::descriptor::prepare_query`]).
    ///
    /// # Returns
    /// Einen Vektor je Eingabetext, in derselben Reihenfolge wie `texts`.
    ///
    /// # Errors
    /// - [`EmbedError`]: implementierungsabhängig (z. B. ein Netzfehler bei
    ///   einem entfernten Backend). [`DeterministicEmbedder`] schlägt nie
    ///   fehl.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// Die Anzahl der Dimensionen, die jeder von [`Embedder::embed`]
    /// gelieferte Vektor haben muss.
    ///
    /// # Returns
    /// Die erwartete Vektorlänge dieses Embedders.
    fn dimensions(&self) -> usize;

    /// Wo dieser Embedder tatsächlich rechnet.
    ///
    /// # Description
    /// Default-Methode, die auf [`Locality::Remote`] fehlschließt -- siehe
    /// den `# Embedder::locality`-Abschnitt der Moduldokumentation für die
    /// Begründung dieser Richtung. Implementierungen, die tatsächlich lokal
    /// rechnen (z. B. [`DeterministicEmbedder`]), überschreiben diese
    /// Methode ausdrücklich, statt sich auf einen impliziten Default zu
    /// verlassen.
    ///
    /// # Returns
    /// [`Locality::Local`], wenn dieser Embedder nachweislich nie den Host
    /// verlässt; sonst [`Locality::Remote`] (Default).
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_embed::{DeterministicEmbedder, Embedder};
    /// use harw_lens_types::Locality;
    ///
    /// assert_eq!(DeterministicEmbedder::new(4).locality(), Locality::Local);
    /// ```
    fn locality(&self) -> Locality {
        Locality::Remote
    }
}

/// Deterministischer Embedder für Tests und Fixtures.
///
/// # Description
/// Erzeugt aus einem `blake3`-Hash des Textes einen Vektor fester Länge --
/// deterministisch, ohne Netz, ohne Modell. Kein Wegwerf-Mock: die
/// Fixture-Tests des Index (`harw-lens-index`, nachgelagert) brauchen genau
/// dieses Verhalten, um einen Index ohne echtes Modell aufzubauen.
#[derive(Debug, Clone, Copy)]
pub struct DeterministicEmbedder {
    dimensions: usize,
}

impl DeterministicEmbedder {
    /// Baut einen deterministischen Embedder mit fester Dimensionszahl.
    ///
    /// # Arguments
    /// - `dimensions` (`usize`): die Länge, die jeder erzeugte Vektor hat.
    ///
    /// # Returns
    /// Ein neuer `DeterministicEmbedder`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_embed::DeterministicEmbedder;
    /// use harw_lens_embed::Embedder;
    ///
    /// let embedder = DeterministicEmbedder::new(8);
    /// assert_eq!(embedder.dimensions(), 8);
    /// ```
    #[must_use]
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }

    /// Leitet einen einzelnen deterministischen Vektor aus einem Text ab.
    ///
    /// # Description
    /// Erweitert den `blake3`-Hash von `text` per Zähler-Suffix zu einem
    /// Strom aus Bytes, bis genug Bytes für `self.dimensions` `f32`-Werte
    /// vorliegen. Zwei gleiche Texte liefern immer denselben Vektor;
    /// unterschiedliche Texte liefern (praktisch sicher) unterschiedliche
    /// Vektoren.
    fn vector_for(&self, text: &str) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.dimensions);
        let mut counter: u64 = 0;
        while out.len() < self.dimensions {
            let mut hasher = blake3::Hasher::new();
            hasher.update(text.as_bytes());
            hasher.update(&counter.to_le_bytes());
            let hash = hasher.finalize();
            for chunk in hash.as_bytes().chunks_exact(4) {
                if out.len() == self.dimensions {
                    break;
                }
                let bytes: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
                // Auf [0, 1) abgebildet: reicht für einen Test-Fixture-Vektor,
                // ohne dass diese Crate eine Zufallszahlen-Abhängigkeit braucht.
                let value = u32::from_le_bytes(bytes);
                out.push((value as f64 / u64::from(u32::MAX) as f64) as f32);
            }
            counter += 1;
        }
        out
    }
}

impl Embedder for DeterministicEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|text| self.vector_for(text)).collect())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Rechnet ausschließlich in-process aus einem `blake3`-Hash -- verlässt
    /// den Host nie. Überschreibt den [`Embedder::locality`]-Default
    /// ausdrücklich, statt sich auf ihn zu verlassen (siehe die Moduldoku).
    fn locality(&self) -> Locality {
        Locality::Local
    }
}

/// Prüft die Dimensionszahl eines umschlossenen [`Embedder`] bei jedem
/// Aufruf.
///
/// # Description
/// Ein stillschweigend falsch dimensionierter Vektor macht einen ganzen
/// Index unbrauchbar, und der Fehler zeigt sich erst bei der Abfrage. Dieser
/// Wrapper macht die Prüfung zur Aufrufzeit statt bei der späteren Abfrage:
/// jeder von `inner.embed` gelieferte Vektor wird gegen `inner.dimensions()`
/// geprüft, bevor er den Aufrufer erreicht.
pub struct DimensionCheckedEmbedder<E: Embedder> {
    inner: E,
}

impl<E: Embedder> DimensionCheckedEmbedder<E> {
    /// Umschließt einen `Embedder` mit einer Dimensionsprüfung.
    ///
    /// # Arguments
    /// - `inner` (`E`): der zu prüfende Embedder.
    ///
    /// # Returns
    /// Ein neuer `DimensionCheckedEmbedder<E>`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_embed::{DeterministicEmbedder, DimensionCheckedEmbedder, Embedder};
    ///
    /// let checked = DimensionCheckedEmbedder::new(DeterministicEmbedder::new(8));
    /// assert!(checked.embed(&["hello".to_owned()]).is_ok());
    /// ```
    #[must_use]
    pub fn new(inner: E) -> Self {
        Self { inner }
    }
}

impl<E: Embedder> Embedder for DimensionCheckedEmbedder<E> {
    /// Bettet ein und prüft danach jeden Vektor gegen `inner.dimensions()`.
    ///
    /// # Errors
    /// - [`EmbedError::DimensionMismatch`]: wenn `inner` einen Vektor
    ///   liefert, dessen Länge von `inner.dimensions()` abweicht.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let vectors = self.inner.embed(texts)?;
        let expected = self.inner.dimensions();
        for vector in &vectors {
            if vector.len() != expected {
                return Err(EmbedError::DimensionMismatch {
                    expected,
                    actual: vector.len(),
                });
            }
        }
        Ok(vectors)
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    /// Delegiert unverändert an `inner`: dieser Wrapper prüft nur
    /// Dimensionen, er verändert nie, wo eingebettet wird.
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_deterministic_embedder_same_text_yields_same_vector() -> TestResult {
        let embedder = DeterministicEmbedder::new(16);
        let a = embedder
            .embed(&["hello".to_owned()])
            .map_err(ctx("no error"))?;
        let b = embedder
            .embed(&["hello".to_owned()])
            .map_err(ctx("no error"))?;
        assert_eq!(a, b);
        Ok(())
    }

    #[test]
    fn test_deterministic_embedder_different_texts_yield_different_vectors() -> TestResult {
        let embedder = DeterministicEmbedder::new(16);
        let a = embedder
            .embed(&["hello".to_owned()])
            .map_err(ctx("no error"))?;
        let b = embedder
            .embed(&["world".to_owned()])
            .map_err(ctx("no error"))?;
        assert_ne!(a, b);
        Ok(())
    }

    #[test]
    fn test_deterministic_embedder_vector_length_matches_dimensions() -> TestResult {
        let embedder = DeterministicEmbedder::new(24);
        let vectors = embedder
            .embed(&["a".to_owned(), "b".to_owned()])
            .map_err(ctx("no error"))?;
        for vector in vectors {
            assert_eq!(vector.len(), 24);
        }
        Ok(())
    }

    /// Ein `Embedder`, der bewusst eine falsche Vektorlänge liefert -- der
    /// Fall, den [`DimensionCheckedEmbedder`] erkennen muss.
    struct LyingEmbedder;

    impl Embedder for LyingEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            Ok(texts.iter().map(|_| vec![0.0_f32; 4]).collect())
        }

        fn dimensions(&self) -> usize {
            8
        }
    }

    #[test]
    fn test_dimension_checked_embedder_detects_wrong_vector_length() {
        let checked = DimensionCheckedEmbedder::new(LyingEmbedder);
        let result = checked.embed(&["hello".to_owned()]);
        assert_eq!(
            result,
            Err(EmbedError::DimensionMismatch {
                expected: 8,
                actual: 4,
            })
        );
    }

    #[test]
    fn test_dimension_checked_embedder_passes_through_correct_vectors() {
        let checked = DimensionCheckedEmbedder::new(DeterministicEmbedder::new(8));
        let result = checked.embed(&["hello".to_owned()]);
        assert!(result.is_ok());
        assert_eq!(checked.dimensions(), 8);
    }

    #[test]
    fn test_deterministic_embedder_locality_is_local() {
        assert_eq!(DeterministicEmbedder::new(4).locality(), Locality::Local);
    }

    #[test]
    fn test_dimension_checked_embedder_locality_delegates_to_inner() {
        let checked = DimensionCheckedEmbedder::new(DeterministicEmbedder::new(4));
        assert_eq!(checked.locality(), Locality::Local);
    }

    /// Ein `Embedder`, der `locality()` nicht überschreibt -- der Default
    /// muss auf [`Locality::Remote`] fehlschließen (siehe die Moduldoku).
    struct UnlabeledEmbedder;

    impl Embedder for UnlabeledEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            Ok(texts.iter().map(|_| vec![0.0_f32; 1]).collect())
        }

        fn dimensions(&self) -> usize {
            1
        }
    }

    #[test]
    fn test_embedder_locality_default_fails_closed_to_remote() {
        assert_eq!(UnlabeledEmbedder.locality(), Locality::Remote);
    }
}
