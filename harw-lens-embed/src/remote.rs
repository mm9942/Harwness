//! Die vierte Einbettungsschicht: ein Embedder, der den Host verlässt.
//!
//! # Warum diese Schicht anders behandelt wird als die drei anderen
//! [`crate::spec::EmbeddingSpec`], [`crate::descriptor::EmbeddingDescriptor`]
//! und [`crate::runtime::RuntimeProfile`] beschreiben ein Modell, gleich ob
//! es lokal oder entfernt rechnet -- reine Daten, ohne Risiko für sich
//! genommen. [`RemoteEmbedder`] ist etwas anderes: er ist der Code, der den
//! präfixierten Chunktext tatsächlich verschickt. Ab dem Moment, in dem
//! [`Embedder::embed`] auf einem `RemoteEmbedder` läuft, hat der Text den
//! Vertrauensbereich dieses Hosts verlassen -- **unwiderruflich**. Ein Bug in
//! einem lokalen Embedder (z. B. eine falsche Dimension) ist korrigierbar,
//! sobald er auffällt; ein Bug, der vertrauliches Material an einen
//! entfernten Dienst schickt, ist es nicht: die Daten sind dort, sobald der
//! Aufruf abgeschickt wurde, unabhängig davon, ob der Fehler je bemerkt wird.
//! Das ist der Grund, warum diese Datei die einzige in `harw-lens-embed`
//! ist, deren zentraler Typ [`Embedder::locality`] hart auf
//! [`Locality::Remote`] fixiert -- nicht konfigurierbar, nicht vom Aufrufer
//! überschreibbar (siehe unten).
//!
//! # Die Transportabstraktion, und ihr erster echter Transport (Knoten AW7-06)
//! [`RemoteEmbedder`] löst den Transport über eine Abstraktion:
//! [`RemoteEmbedBackend`] kennt nur „Texte rein, Vektoren raus (oder ein
//! Fehler)" -- wie das geschieht (HTTP, gRPC, ein In-Process-Aufzeichner für
//! Tests) ist Sache der Implementierung. Bis Knoten AW7-06 gab es dafür genau
//! eine Implementierung: [`RecordingBackend`] (`#[cfg(test)]` unten), die
//! beweist, dass [`RemoteEmbedder`] korrekt verdrahtet ist, ohne dass ein
//! Test je eine echte Netzverbindung aufbaut. Seit AW7-06 kommt
//! [`crate::http_backend::HttpEmbedBackend`] hinzu -- der erste
//! `RemoteEmbedBackend`, der tatsächlich über HTTP verschickt. Er liegt
//! **in dieser Crate selbst**, nicht in `harw-provider`/`harw-provider-http`:
//! jene Crate bot zum Zeitpunkt von AW7-06 keinen Einbettungs-Aufruf an
//! (siehe [`crate::http_backend`]s Moduldokumentation für den vollständigen
//! Befund und die Abwägung gegen ein lokales ML-Modell als Alternative).
//!
//! # Warum `locality()` nicht vom Backend abhängt
//! [`RemoteEmbedder::locality`] liefert `Locality::Remote`, unabhängig davon,
//! was das konkrete `B: RemoteEmbedBackend` tatsächlich tut -- selbst wenn
//! ein Test-Backend in Wirklichkeit nur eine `HashMap` befragt. Der Typ
//! `RemoteEmbedder<B>` *ist* die Erklärung „dieser Text verlässt den Host",
//! unabhängig vom Backend, das sie einlöst. Ein `locality()`, das an das
//! Backend delegierte, machte diese Erklärung zu einer Laufzeitprüfung, die
//! ein Backend falsch beantworten könnte (versehentlich oder böswillig);
//! als Typ-Konstante kann sie das nicht. `harw-lens-source::build_index`
//! liest genau dieses Signal, um `operator-only`-Sichtbarkeits-Buckets vor
//! `RemoteEmbedder` zu schützen (Nullzähler
//! `lens_remote_embed_on_operator_only`, siehe dessen Moduldoku).
//!
//! # Nebenläufigkeit
//! [`RemoteEmbedBackend`] verlangt `Send + Sync`, genau wie [`Embedder`]
//! selbst: Implementierungen werden hinter `&dyn Embedder` geteilt und aus
//! mehreren Threads aufgerufen.
//!
//! # Fehler
//! [`crate::error::EmbedError::RemoteBackendFailed`], wenn ein
//! [`RemoteEmbedBackend`] keine Vektoren liefern kann (Transportfehler,
//! ungültige Antwort, …). Das konkrete Transportfehlerformat steht noch
//! nicht fest, deshalb trägt die Variante nur eine bereits formatierte
//! Beschreibung, kein gewickeltes Fremdformat.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{Embedder, RemoteEmbedBackend, RemoteEmbedder};
//! use harw_lens_types::Locality;
//!
//! struct FixedVectorBackend;
//! impl RemoteEmbedBackend for FixedVectorBackend {
//!     fn embed_remote(
//!         &self,
//!         texts: &[String],
//!     ) -> Result<Vec<Vec<f32>>, harw_lens_embed::EmbedError> {
//!         Ok(texts.iter().map(|_| vec![0.0_f32; 4]).collect())
//!     }
//! }
//!
//! let embedder = RemoteEmbedder::new(FixedVectorBackend, 4);
//! assert_eq!(embedder.locality(), Locality::Remote);
//! let vectors = embedder.embed(&["secret".to_owned()]).expect("backend succeeds");
//! assert_eq!(vectors[0].len(), 4);
//! ```

use harw_lens_types::Locality;

use crate::embedder::Embedder;
use crate::error::EmbedError;

/// Transportabstraktion für [`RemoteEmbedder`]: der tatsächliche Aufruf
/// eines entfernten Einbettungsdienstes.
///
/// # Description
/// Kennt nur „Texte rein, Vektoren raus (oder ein Fehler)" -- kein Protokoll
/// im Trait selbst festgelegt. [`crate::http_backend::HttpEmbedBackend`]
/// implementiert diesen Trait seit Knoten AW7-06 über HTTP; Tests
/// implementieren ihn stattdessen mit einem aufzeichnenden In-Process-Backend,
/// das nie eine Netzverbindung aufbaut.
///
/// # Concurrency
/// `Send + Sync`, wie [`Embedder`] selbst.
pub trait RemoteEmbedBackend: Send + Sync {
    /// Schickt `texts` an das entfernte Modell und liefert einen Vektor je
    /// Text, in derselben Reihenfolge.
    ///
    /// # Arguments
    /// - `texts` (`&[String]`): die bereits präfixierten Texte.
    ///
    /// # Returns
    /// Einen Vektor je Eingabetext, in derselben Reihenfolge wie `texts`.
    ///
    /// # Errors
    /// - [`EmbedError::RemoteBackendFailed`]: wenn der Dienst nicht
    ///   antwortet oder eine unbrauchbare Antwort liefert.
    fn embed_remote(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// Die vierte Einbettungsschicht: ein [`Embedder`], der Text an ein
/// entferntes Modell schickt.
///
/// # Description
/// Reiner Umschlag um ein [`RemoteEmbedBackend`] plus die Dimensionszahl, die
/// das Backend verspricht. [`RemoteEmbedder::locality`] liefert immer
/// [`Locality::Remote`] -- siehe den `# Warum locality() nicht vom Backend
/// abhängt`-Abschnitt der Moduldokumentation.
///
/// # Concurrency
/// `Send + Sync`, solange `B` es ist (per `RemoteEmbedBackend`-Bound
/// garantiert).
pub struct RemoteEmbedder<B: RemoteEmbedBackend> {
    backend: B,
    dimensions: usize,
}

impl<B: RemoteEmbedBackend> RemoteEmbedder<B> {
    /// Baut einen `RemoteEmbedder` um ein konkretes Backend.
    ///
    /// # Arguments
    /// - `backend` (`B`): die Transportimplementierung.
    /// - `dimensions` (`usize`): die Vektorlänge, die `backend` liefert.
    ///
    /// # Returns
    /// Einen neuen `RemoteEmbedder<B>`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_embed::{RemoteEmbedBackend, RemoteEmbedder};
    ///
    /// struct EmptyBackend;
    /// impl RemoteEmbedBackend for EmptyBackend {
    ///     fn embed_remote(
    ///         &self,
    ///         texts: &[String],
    ///     ) -> Result<Vec<Vec<f32>>, harw_lens_embed::EmbedError> {
    ///         Ok(texts.iter().map(|_| vec![0.0_f32; 2]).collect())
    ///     }
    /// }
    /// let embedder = RemoteEmbedder::new(EmptyBackend, 2);
    /// // `dimensions` ist ein privates Feld, kein Verfahren; die Zahl kommt
    /// // über den `Embedder`-Trait heraus.
    /// use harw_lens_embed::Embedder as _;
    /// assert_eq!(embedder.dimensions(), 2);
    /// ```
    #[must_use]
    pub fn new(backend: B, dimensions: usize) -> Self {
        Self {
            backend,
            dimensions,
        }
    }
}

impl<B: RemoteEmbedBackend> Embedder for RemoteEmbedder<B> {
    /// Reicht `texts` unverändert an `backend.embed_remote` weiter.
    ///
    /// # Errors
    /// - [`EmbedError::RemoteBackendFailed`]: siehe [`RemoteEmbedBackend::embed_remote`].
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.backend.embed_remote(texts)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Immer [`Locality::Remote`] -- siehe die Moduldokumentation, Abschnitt
    /// „Warum `locality()` nicht vom Backend abhängt".
    fn locality(&self) -> Locality {
        Locality::Remote
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::test_support::{TestResult, ctx};

    /// Aufzeichnendes Test-Backend: hält jeden `embed_remote`-Aufruf fest,
    /// statt eine echte Netzverbindung aufzubauen. Das war zum Zeitpunkt von
    /// Knoten AW7-05 der einzige `RemoteEmbedBackend`; der erste echte
    /// Transport ([`crate::http_backend::HttpEmbedBackend`]) kam erst in
    /// Knoten AW7-06 hinzu (siehe dessen Moduldoku).
    struct RecordingBackend {
        dimensions: usize,
        calls: AtomicUsize,
        seen_texts: Mutex<Vec<String>>,
        fail: bool,
    }

    impl RecordingBackend {
        fn new(dimensions: usize) -> Self {
            Self {
                dimensions,
                calls: AtomicUsize::new(0),
                seen_texts: Mutex::new(Vec::new()),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                dimensions: 0,
                calls: AtomicUsize::new(0),
                seen_texts: Mutex::new(Vec::new()),
                fail: true,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn texts_seen(&self) -> TestResult<Vec<String>> {
            Ok(self
                .seen_texts
                .lock()
                .map_err(ctx("Mutex vergiftet"))?
                .clone())
        }
    }

    impl RemoteEmbedBackend for RecordingBackend {
        fn embed_remote(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_texts
                .lock()
                .map_err(|error| EmbedError::RemoteBackendFailed {
                    reason: format!("lock poisoned: {error}"),
                })?
                .extend(texts.iter().cloned());
            if self.fail {
                return Err(EmbedError::RemoteBackendFailed {
                    reason: "simulated transport failure".to_owned(),
                });
            }
            Ok(texts
                .iter()
                .map(|_| vec![0.0_f32; self.dimensions])
                .collect())
        }
    }

    #[test]
    fn test_remote_embedder_locality_is_always_remote() {
        let embedder = RemoteEmbedder::new(RecordingBackend::new(4), 4);
        assert_eq!(embedder.locality(), Locality::Remote);
    }

    #[test]
    fn test_remote_embedder_forwards_texts_to_backend() -> TestResult {
        let embedder = RemoteEmbedder::new(RecordingBackend::new(4), 4);
        let vectors = embedder
            .embed(&["a".to_owned(), "b".to_owned()])
            .map_err(ctx("backend succeeds"))?;
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0].len(), 4);
        Ok(())
    }

    #[test]
    fn test_recording_backend_records_exactly_the_texts_it_was_asked_to_embed() -> TestResult {
        // Proves the recording test double itself is trustworthy: the
        // guarantee `harw-lens-source`'s tests rely on ("this text never
        // reached the remote backend") is only meaningful if the backend
        // actually records what it is asked to embed.
        let backend = RecordingBackend::new(2);
        backend
            .embed_remote(&["hello".to_owned(), "world".to_owned()])
            .map_err(ctx("backend succeeds"))?;
        assert_eq!(backend.calls(), 1);
        assert_eq!(
            backend.texts_seen()?,
            vec!["hello".to_owned(), "world".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn test_remote_embedder_forwarding_is_transparent_to_call_count() -> TestResult {
        // `RemoteEmbedder::embed` must not add, drop, or batch calls beyond
        // a single forward to the backend.
        struct CountingWrapper {
            inner: RecordingBackend,
        }
        impl RemoteEmbedBackend for CountingWrapper {
            fn embed_remote(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
                self.inner.embed_remote(texts)
            }
        }
        let embedder = RemoteEmbedder::new(
            CountingWrapper {
                inner: RecordingBackend::new(3),
            },
            3,
        );
        embedder
            .embed(&["one".to_owned(), "two".to_owned(), "three".to_owned()])
            .map_err(ctx("backend succeeds"))?;
        assert_eq!(embedder.backend.inner.calls(), 1);
        assert_eq!(
            embedder.backend.inner.texts_seen()?,
            vec!["one".to_owned(), "two".to_owned(), "three".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn test_remote_embedder_propagates_backend_failure() {
        let embedder = RemoteEmbedder::new(RecordingBackend::failing(), 0);
        let result = embedder.embed(&["x".to_owned()]);
        assert_eq!(
            result,
            Err(EmbedError::RemoteBackendFailed {
                reason: "simulated transport failure".to_owned(),
            })
        );
    }

    #[test]
    fn test_remote_embedder_dimensions_matches_constructor_argument() {
        let embedder = RemoteEmbedder::new(RecordingBackend::new(7), 7);
        assert_eq!(embedder.dimensions(), 7);
    }
}
