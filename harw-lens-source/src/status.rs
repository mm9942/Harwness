//! Introspektion eines bereits gebauten Index: [`IndexStatus`]/[`index_status`].
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`IndexStatus`] und [`index_status`] — die
//! lesende Ergänzung zu [`crate::build_index`]: keine Chunks, keine
//! Vektoren, nur die Frage „existiert dieser Index, und was behauptet er
//! über sich selbst". Das ist genau die Introspektionsfunktion, die
//! `harw-lens`s eigene Moduldokumentation als bewusste künftige Ergänzung
//! ankündigt (Abschnitt zu `IndexManifest` in `harw-lens/src/lib.rs`: „Eine
//! künftige Introspektionsfunktion (z. B. „welches Modell hat Index X
//! gebaut") wäre eine bewusste neue Ergänzung, kein vergessener Fall.").
//! Erster Konsument ist `harw lens status` (`harw-cli`).
//!
//! # Warum über [`FlatIndex::load`] statt nur über das Manifest
//! [`harw_lens_store::LensStore::get_index_manifest`] allein liefert
//! `model`, `locality`, `chunker_version`, `metric`, `visibility` und
//! `source_set_digest` — aber weder die Embedding-Dimension noch die
//! Anzahl gespeicherter Chunks: beides sind Eigenschaften der tatsächlich
//! gespeicherten Einträge, keine Manifestfelder (siehe
//! [`harw_lens_types::IndexManifest`]s Moduldokumentation). [`index_status`]
//! lädt deshalb den vollständigen [`FlatIndex`] über [`FlatIndex::load`] —
//! dieselbe Funktion, die auch eine Abfrage laden würde — statt das
//! Ablageformat ein zweites Mal, unvollständig, selbst zu parsen.
//!
//! # Warum kein `built_at`-Feld erfunden wird
//! [`harw_lens_types::IndexManifest`] trägt kein `built_at`; ein solches
//! Feld hinzuzufügen bräche jede bestehende Manifest-Konstruktion in allen
//! zehn Lens-Crates (`#[serde(deny_unknown_fields)]` verlangt zudem eine
//! rückwärtskompatible Migration bereits abgelegter Manifeste) — eine
//! Ausbaustufe, die dieser Knoten nicht rechtfertigt. Stattdessen liefert
//! [`harw_lens_store::LensStore::index_manifest_modified`] die
//! Dateisystem-`mtime` von `manifest.json` als Altersnäherung; siehe dessen
//! Dokumentation für die Begründung, warum das für eine Status-Auskunft
//! ausreicht.
//!
//! # Nebenläufigkeit
//! [`index_status`] tut ausschließlich lesendes I/O (keine Lock-Datei, wie
//! [`harw_lens_store::LensStore::get_index_manifest`]/
//! [`FlatIndex::load`]); sicher parallel zu einem laufenden
//! [`crate::build_index`] desselben Index aufrufbar, mit derselben
//! Einschränkung, die dessen Moduldokumentation für gleichzeitige Leser
//! nennt (ein atomar ausgetauschtes Manifest/Datenpaar wird entweder
//! vollständig alt oder vollständig neu gelesen, nie halb).
//!
//! # Fehler
//! [`SourceResult`]; siehe [`index_status`] für die vollständige
//! Variantenliste.

use std::path::Path;
use std::time::SystemTime;

use harw_lens_index::{FlatIndex, VectorIndex};
use harw_lens_store::LensStore;
use harw_lens_types::{Locality, Metric};

use crate::error::SourceResult;

/// Was ein bereits gebauter Index über sich selbst preisgibt, ohne dass ein
/// Aufrufer ihn dafür abfragen müsste.
///
/// # Description
/// Reine Daten, zusammengetragen aus [`harw_lens_types::IndexManifest`]
/// (`model`, `locality`, `metric`, `chunker_version`), dem tatsächlich
/// gespeicherten [`FlatIndex`] (`dimension`, `chunk_count`) und der
/// Dateisystem-`mtime` seines Manifests (`modified`, siehe
/// [`harw_lens_store::LensStore::index_manifest_modified`]).
#[derive(Debug, Clone, PartialEq)]
pub struct IndexStatus {
    /// Name des Index (z. B. [`crate::DOCS_DESIGN_INDEX`]).
    pub index_name: String,
    /// Sichtbarkeits-Bucket, in dem dieser Index liegt.
    pub visibility: String,
    /// Name/Kennung des Embedding-Modells, mit dem gebaut wurde.
    pub model: String,
    /// Wo das Embedding berechnet wurde.
    pub locality: Locality,
    /// Abstandsmaß des Index.
    pub metric: Metric,
    /// Version des Chunkers, mit dem gebaut wurde.
    pub chunker_version: u32,
    /// Embedding-Länge; `None` für einen leeren Index (siehe
    /// [`FlatIndex::dimension`]).
    pub dimension: Option<usize>,
    /// Anzahl gespeicherter (Chunk, Embedding)-Einträge (siehe
    /// [`FlatIndex::len`]).
    pub chunk_count: usize,
    /// Dateisystem-`mtime` des Manifests — eine Altersnäherung, kein
    /// mitgeführtes Bau-Zeitfeld (siehe den `//!`-Block dieses Moduls).
    pub modified: Option<SystemTime>,
}

/// Liest den Status eines bereits gebauten Index, ohne ihn zu verändern.
///
/// # Arguments
/// - `home` (`&Path`): der Root-Space, unter dem
///   [`harw_home::paths::visibility_index_dir`] aufgelöst wird — derselbe
///   Parameter wie bei [`crate::build_index`].
/// - `index_name` (`&str`): der Indexname (z. B.
///   [`crate::DOCS_DESIGN_INDEX`]/[`crate::KNOWLEDGE_PALACE_INDEX`]).
/// - `visibility` (`&str`): der Sichtbarkeits-Bucket-Name (z. B.
///   [`crate::DEFAULT_VISIBILITY`]/[`crate::OPERATOR_ONLY_VISIBILITY`]).
///
/// # Returns
/// `Some(IndexStatus)`, wenn unter `(index_name, visibility)` bereits ein
/// Index gebaut wurde; `None`, wenn nicht — ein fehlender Index ist ein
/// normaler Zustand, kein Fehler (wie bei
/// [`harw_lens_store::LensStore::get_index_manifest`]).
///
/// # Errors
/// - [`crate::SourceError::Home`]: `visibility` ist kein gültiger
///   Sichtbarkeits-Bucket-Name.
/// - [`crate::SourceError::Store`]: ein Speicherfehler beim Lesen des
///   Manifests oder seiner `mtime`.
/// - [`crate::SourceError::Index`]: der gespeicherte [`FlatIndex`] ist
///   beschädigt (z. B. [`harw_lens_index::IndexError::PersistedManifestMismatch`]).
///
/// # Examples
/// ```rust,no_run
/// use harw_lens_source::{index_status, DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX};
///
/// let home = tempfile::tempdir()?;
/// match index_status(home.path(), DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY)? {
///     Some(status) => println!("{} Chunks, Modell {}", status.chunk_count, status.model),
///     None => println!("noch nicht gebaut"),
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn index_status(
    home: &Path,
    index_name: &str,
    visibility: &str,
) -> SourceResult<Option<IndexStatus>> {
    let store_root = harw_home::paths::visibility_index_dir(home, visibility)?;
    let store = LensStore::open(&store_root)?;

    if store.get_index_manifest(index_name)?.is_none() {
        return Ok(None);
    }

    let modified = store.index_manifest_modified(index_name)?;
    let index = FlatIndex::load(&store, index_name)?;
    let manifest = index.manifest();

    Ok(Some(IndexStatus {
        index_name: index_name.to_owned(),
        visibility: visibility.to_owned(),
        model: manifest.model.clone(),
        locality: manifest.locality,
        metric: manifest.metric,
        chunker_version: manifest.chunker_version,
        dimension: index.dimension(),
        chunk_count: index.len(),
        modified,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
    use harw_lens_types::{Locality as Loc, SourceRef};

    use crate::{DEFAULT_VISIBILITY, RawDocument, build_index};

    fn descriptor() -> EmbeddingDescriptor {
        EmbeddingDescriptor {
            document_prefix: "passage: ".to_owned(),
            query_prefix: "query: ".to_owned(),
            normalize: false,
        }
    }

    #[test]
    fn test_index_status_returns_none_for_never_built_index() -> TestResult {
        let home = tempfile::tempdir()?;
        let status = index_status(home.path(), "docs.design", DEFAULT_VISIBILITY)?;
        assert_eq!(status, None);
        Ok(())
    }

    #[test]
    fn test_index_status_reports_model_locality_and_chunk_count_after_build() -> TestResult {
        let home = tempfile::tempdir()?;
        let documents = vec![RawDocument {
            source: SourceRef::File {
                path: "intro.md".to_owned(),
            },
            text: "# Intro\n\nEin Absatz Text.\n".to_owned(),
            visibility: DEFAULT_VISIBILITY.to_owned(),
            is_binary: false,
            content_hash: None,
        }];
        let embedder = DeterministicEmbedder::new(8);
        build_index(
            home.path(),
            "docs.design",
            &documents,
            "test-model",
            Loc::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )?;

        let status = index_status(home.path(), "docs.design", DEFAULT_VISIBILITY)?
            .ok_or(TestError::Missing("index exists after build"))?;
        assert_eq!(status.index_name, "docs.design");
        assert_eq!(status.visibility, DEFAULT_VISIBILITY);
        assert_eq!(status.model, "test-model");
        assert_eq!(status.locality, Loc::Local);
        assert_eq!(status.metric, Metric::Cosine);
        assert_eq!(status.dimension, Some(8));
        assert!(status.chunk_count > 0);
        assert!(status.modified.is_some());
        Ok(())
    }

    #[test]
    fn test_index_status_rejects_invalid_visibility_name() -> TestResult {
        let home = tempfile::tempdir()?;
        let result = index_status(home.path(), "docs.design", "../escape");
        assert!(matches!(result, Err(crate::SourceError::Home(_))));
        Ok(())
    }
}
