//! `build`: baut oder aktualisiert einen Index über die Fassade.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`build`] — eine reine Weiterleitung an
//! [`harw_lens_source::build_index`], ohne eigene Logik. Diese Datei
//! verändert weder Parameter noch Ergebnis; sie übersetzt nur den
//! Fehlertyp ([`harw_lens_source::SourceError`] zu [`crate::LensError`],
//! über `#[from]` in [`crate::LensError::Source`]).
//!
//! # Warum keine eigene Signatur-Vereinfachung
//! Die Signatur von [`build`] ist identisch zu
//! [`harw_lens_source::build_index`] bis auf den Fehlertyp. Eine Fassade,
//! die hier z. B. `model`/`locality`/`metric` bündelt oder den Embedder
//! selbst aus einer Rolle auswählt, würde eine Entscheidung treffen
//! (welches Modell, welche Lokalität), die dem Aufrufer gehört — siehe
//! den `//!`-Block von `crate` für die Begründung, warum
//! [`crate::EmbeddingCatalog`]/[`crate::route`]/[`crate::EmbeddingRole`]
//! stattdessen als eigene, vom Aufrufer selbst zu bedienende Bausteine
//! Teil der Fassadenfläche sind, statt in `build` versteckt zu werden.
//!
//! # Nebenläufigkeit
//! [`build`] hält keinen Zustand zwischen Aufrufen; siehe die
//! Nebenläufigkeitsgarantien von [`harw_lens_source::build_index`] für die
//! vollständige Zusage (u. a. Serialisierung gleichzeitiger Aufrufe für
//! dasselbe `(home, index_name, visibility)`-Tripel über eine
//! `fs4`-Advisory-Lock).
//!
//! # Fehler
//! Siehe [`crate::LensError`].

use std::path::Path;

use harw_lens_embed::{Embedder, EmbeddingDescriptor};
use harw_lens_source::{build_index, IndexBuildReport, RawDocument};
use harw_lens_types::{Locality, Metric};

use crate::LensResult;

/// Baut oder aktualisiert einen Index — einen physisch getrennten
/// Vektorindex (intern `harw_lens_index::FlatIndex`) je Sichtbarkeits-Bucket,
/// der in `documents` vorkommt.
///
/// # Description
/// Reine Weiterleitung an [`harw_lens_source::build_index`]: gruppiert
/// `documents` nach [`RawDocument::visibility`], zerlegt sie (deterministisch,
/// über `harw-lens-chunk`, innerhalb von `harw-lens-source`) und bettet nur
/// tatsächlich neue Chunks ein (inkrementell über den in `harw-lens-source`
/// geführten Embedding-Cache). Diese Fassade fügt dabei **nichts** hinzu —
/// kein zweites Chunking, keine zweite Kostenrechnung, keine eigene
/// Persistenz.
///
/// # Arguments
/// - `home` (`&Path`): der Root-Space, unter dem
///   `harw_home::paths::visibility_index_dir` je Sichtbarkeits-Bucket
///   aufgelöst wird.
/// - `index_name` (`&str`): der Name des zu bauenden Index (z. B.
///   [`crate::DOCS_DESIGN_INDEX`], [`crate::KNOWLEDGE_PALACE_INDEX`]).
/// - `documents` (`&[RawDocument]`): die noch nicht zerlegten Quellen,
///   typischerweise aus [`crate::collect_design_docs`] oder
///   [`crate::collect_palace_documents`].
/// - `model` (`&str`): Name/Kennung des Embedding-Modells, ins gebaute
///   Manifest geschrieben.
/// - `locality` (`Locality`): wo das Embedding berechnet wurde, ebenfalls
///   Teil des Manifests.
/// - `metric` (`Metric`): das Abstandsmaß des gebauten Index.
/// - `embedder` (`&dyn Embedder`): berechnet die Vektoren für neue Chunks.
/// - `descriptor` (`&EmbeddingDescriptor`): liefert das Dokument-Präfix, das
///   vor dem Einbetten angewendet wird.
///
/// # Returns
/// Einen [`IndexBuildReport`] je betroffenem Sichtbarkeits-Bucket, sortiert
/// nach Sichtbarkeitsname.
///
/// # Errors
/// - [`crate::LensError::Source`]: siehe [`harw_lens_source::build_index`]
///   für die vollständige Variantenliste des gewickelten
///   [`harw_lens_source::SourceError`] (u. a. fehlerhafte
///   Sichtbarkeitsnamen, Speicher-/Indexfehler, Einbettungsfehler,
///   Dateisystem-/JSON-Fehler des internen Embedding-Caches).
///
/// # Examples
/// ```rust,no_run
/// use harw_lens::{build, RawDocument, DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX};
/// use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
/// use harw_lens_types::{Locality, Metric, SourceRef};
///
/// let home = tempfile::tempdir()?;
/// let documents = vec![RawDocument {
///     source: SourceRef::File { path: "intro.md".to_owned() },
///     text: "# Intro\n\nText.\n".to_owned(),
///     visibility: DEFAULT_VISIBILITY.to_owned(),
/// }];
/// let descriptor = EmbeddingDescriptor {
///     document_prefix: "passage: ".to_owned(),
///     query_prefix: "query: ".to_owned(),
///     normalize: false,
/// };
/// let embedder = DeterministicEmbedder::new(16);
///
/// let reports = build(
///     home.path(),
///     DOCS_DESIGN_INDEX,
///     &documents,
///     "test-model",
///     Locality::Local,
///     Metric::Cosine,
///     &embedder,
///     &descriptor,
/// )?;
/// assert_eq!(reports.len(), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[allow(clippy::too_many_arguments)]
pub fn build(
    home: &Path,
    index_name: &str,
    documents: &[RawDocument],
    model: &str,
    locality: Locality,
    metric: Metric,
    embedder: &dyn Embedder,
    descriptor: &EmbeddingDescriptor,
) -> LensResult<Vec<IndexBuildReport>> {
    let reports = build_index(
        home, index_name, documents, model, locality, metric, embedder, descriptor,
    )?;
    Ok(reports)
}
