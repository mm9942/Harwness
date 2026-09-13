//! Inkrementeller Indexaufbau: Chunken, Speichern, selektives Einbetten.
//!
//! # Verantwortungsbereich
//! Besitzt [`IndexBuildReport`] und [`build_index`] — die einzige Stelle
//! dieser Crate, die einen physischen [`harw_lens_index::FlatIndex`] baut und
//! über [`harw_lens_store::LensStore`] ablegt. Alles vor diesem Schritt
//! (welche Dateien/Artefakte, welche Sichtbarkeit) liefert `crate::document`;
//! alles danach (Abfragen, Kollabieren) ist `harw-lens-query`s Aufgabe.
//!
//! # Wie Sichtbarkeit hier physisch getrennt wird
//! [`build_index`] gruppiert die übergebenen [`crate::RawDocument`]s nach
//! `visibility` und öffnet **je Gruppe einen eigenen
//! [`harw_lens_store::LensStore`]**, gewurzelt unter
//! [`harw_home::paths::visibility_index_dir`]. Es gibt keinen gemeinsamen
//! Store, aus dem hinterher gefiltert würde — Chunks einer Sichtbarkeit
//! landen von Anfang an in einem physisch getrennten Verzeichnisbaum. Siehe
//! den `//!`-Block von `crate` für die volle Begründung.
//!
//! # Wie Inkrementalität funktioniert
//! Das Zerlegen (`chunk_markdown`) ist billig und läuft bei jedem Aufruf über
//! das gesamte übergebene Material — deterministisches Chunken bedeutet:
//! unverändertes Material erzeugt exakt dieselben [`harw_lens_types::ChunkDigest`]s
//! wie beim letzten Build. Das Einbetten ist der teure Teil und wird gezielt
//! übersprungen:
//!
//! 1. Für jeden Chunk prüft [`harw_lens_store::LensStore::has_chunk`], ob der
//!    Store diesen Inhalt schon kennt; nur unbekannte Chunks werden per
//!    [`harw_lens_store::LensStore::put_chunk`] neu abgelegt (das Ablegen
//!    selbst ist idempotent, die Prüfung spart trotzdem den Lock/Write-Pfad
//!    für unverändertes Material).
//! 2. Für die eigentliche Einbettung reicht Chunk-Existenz allein nicht: der
//!    Store hält Chunk-Text, aber keine Embeddings. Diese Crate führt daher
//!    einen eigenen, ebenfalls über `LensStore` (unter einem synthetischen
//!    Indexnamen `"<index>.embedding-cache"`) persistierten Digest-zu-Vektor-
//!    Cache. Vor dem Einbetten wird dieser Cache geladen; nur Chunks, deren
//!    Digest darin **fehlt**, werden in einem einzigen Batch an
//!    [`harw_lens_embed::Embedder::embed`] gegeben. Ein zweiter Build über
//!    unverändertes Material findet jeden Digest im Cache wieder und ruft den
//!    Embedder **kein einziges Mal** auf — siehe
//!    `test_build_index_second_build_over_unchanged_material_never_calls_embedder`.
//! 3. Ändert sich eine Quelle, ändert deterministisches Chunken genau die
//!    Digests der betroffenen Chunks; alle anderen bleiben im Cache treffbar.
//!    Es werden deshalb **genau** die betroffenen Chunks neu eingebettet —
//!    siehe `test_build_index_changing_one_document_only_reembeds_its_chunks`.
//! 4. Der Cache ist an `model`/`chunker_version` gebunden: weicht einer der
//!    beiden vom aktuellen Aufruf ab, gilt der geladene Cache als veraltet und
//!    wird verworfen (alles wird neu eingebettet) — ein stiller
//!    Modellwechsel darf nie zu falsch gemischten Vektoren in einem Index
//!    führen.
//!
//! Der fertige [`harw_lens_index::FlatIndex`] wird nach jedem Build
//! vollständig neu aus Chunks **und** (gecachten oder frischen) Embeddings
//! zusammengesetzt und ersetzt den vorherigen — die Inkrementalität betrifft
//! ausschließlich das Einbetten, nicht das Schreiben des Index selbst.
//!
//! # Welche Zerlegungsstrategie ein Index bekommt
//! [`build_visibility_bucket`] wählt die Zerlegungsstrategie anhand von
//! `index_name`, nicht über einen zusätzlichen Parameter: [`build_index`]s
//! Signatur ist mit `harw-lens` (der Fassade, außerhalb des Schreibbereichs
//! dieses Knotens) geteilt und bleibt deshalb unverändert (Auflage „rein
//! additiv"). [`crate::CODE_RUST_INDEX`] wird mit
//! [`harw_lens_chunk::chunk_rust`] zerlegt (Elementgrenzen: `fn`, `struct`,
//! …), jeder andere Indexname weiterhin mit `chunk_markdown`. Eine dritte
//! Strategie, `harw_lens_chunk::chunk_plain`, bekommt in diesem Knoten
//! bewusst **keinen** Konsumenten: keine der neu hinzukommenden Quellen
//! ([`crate::collect_diary_documents`]) ist reiner, unstrukturierter
//! Fließtext ohne Markdown- oder Rust-Struktur — Diary-Einträge sind
//! Markdown-Prosa wie Palace-Knoten. `harw-lens-chunk` bleibt also bei zwei
//! von drei Strategien mit Konsument, festgehalten statt verschwiegen.
//!
//! # Die Falle, die dieser Knoten prüft: strukturell vs. nur geprüft
//! Sichtbarkeit ist in dieser Crate physisch getrennt (siehe oben), aber
//! **welcher Embedder** einen Bucket bedient, entscheidet ausschließlich der
//! Aufrufer — [`build_index`] nimmt `embedder: &dyn Embedder` entgegen und
//! trifft selbst keine Rollen-/Lokalitätsentscheidung (siehe `harw-lens`s
//! `build`-Moduldoku für die Begründung, warum diese Entscheidung dem
//! Aufrufer gehört). Das heißt: nichts im *Typsystem* verhindert, dass ein
//! Aufrufer versehentlich einen Embedder mit
//! [`harw_lens_types::Locality::Remote`] für einen Aufruf übergibt, der auch
//! `operator-only`-sichtbare Dokumente enthält — die Sichtbarkeitstrennung
//! wäre in diesem Fall physisch korrekt (der Chunk landet im richtigen
//! Store), aber der **Text selbst** hätte den Host bereits verlassen, bevor
//! der Store je berührt wurde. [`build_visibility_bucket`] schließt genau
//! diese Lücke: **bevor** irgendein Chunk-Text an `embedder.embed()` geht,
//! prüft es für den `operator-only`-Bucket
//! `embedder.locality() == Locality::Remote` und bricht mit
//! [`crate::SourceError::OperatorOnlyRemoteEmbed`] ab, falls das zutrifft —
//! unabhängig davon, ob es in diesem konkreten Aufruf überhaupt neue Chunks
//! zum Einbetten gäbe (ein leerer `new_texts`-Batch heute schützt nicht vor
//! einem neuen Dokument im selben Bucket morgen). Das ist die
//! strukturelle Durchsetzung, die [`harw_lens_embed::catalog::route`] für
//! `EmbeddingRole::Confidential` bereits hat, hier auf den Aufrufpunkt
//! übertragen, an dem der Text tatsächlich verschickt würde.
//!
//! # Der Nullzähler `lens_remote_embed_on_operator_only`
//! [`LENS_REMOTE_EMBED_ON_OPERATOR_ONLY`] macht den obigen Guard beobachtbar:
//! jeder tatsächliche Auslöser erhöht ihn, **bevor** [`build_visibility_bucket`]
//! den Fehler zurückgibt — der Aufruf ist selbst der Beleg der Verletzung
//! (siehe `harw_observe::null_counter`s Moduldoku). Er sitzt auf einem Pfad,
//! den echte Aufrufe erreichen: [`build_index`] wird nicht nur von den
//! Tests dieser Datei aufgerufen, sondern von `harw-lens::build` (der
//! Fassade, nicht-Test-Code) und von den Integrationstests aus
//! `harw-lens-query` und `harw-lens-federation` — genau die beiden
//! Konsumenten, die dieser Knoten laut Auftrag voraussetzt.
//! `test_build_index_operator_only_bucket_with_remote_embedder_is_rejected`
//! unten treibt ihn absichtlich über null.
//!
//! **Ehrliche Einschränkung:** zum Zeitpunkt dieses Knotens ruft noch kein
//! Agenten-zugewandtes Werkzeug (`harw-tool-lens`, Knoten AW6-10, weiterhin
//! Gerüst) `harw-lens::build` tatsächlich zur Laufzeit auf — die Kette
//! endet an einer echten, nicht-Test-Funktion (`harw-lens::build`), erreicht
//! aber noch keinen Agenten. Der Zähler zählt deshalb heute ausschließlich
//! Verstöße, die ein Test oder ein zukünftiger Aufrufer von `build_index`
//! auslöst — nicht null, weil niemand ihn erreicht (die Falle aus der
//! Moduldoku von `harw_observe::null_counter`), sondern null, solange kein
//! Aufrufer (Test oder `harw-lens::build`) je einen Remote-Embedder für
//! einen `operator-only`-Bucket übergibt.
//!
//! **Und die schärfere Frage:** ist die Trennung damit strukturell oder nur
//! geprüft? **Beides, an unterschiedlichen Stellen.** Für
//! `EmbeddingRole::Confidential` ist sie strukturell auf Ebene des Routers
//! ([`harw_lens_embed::catalog::route`] kann kein entferntes Profil für
//! diese Rolle zurückgeben, ohne dass ein Fehler entsteht). Für die
//! `operator-only`-*Sichtbarkeit* — die dieser Crate eigene, von
//! `EmbeddingRole` unabhängige Achse — ist sie eine **geprüfte** Invariante:
//! [`build_visibility_bucket`] verlangt sie zur Aufrufzeit, aber ein
//! Aufrufer könnte sie umgehen, indem er `harw_lens_embed::Embedder` direkt
//! implementiert und `locality()` fälschlich `Local` zurückgeben lässt —
//! diese Crate hat keine Möglichkeit, das zu verifizieren, weil `locality()`
//! eine reine Selbstauskunft ist, keine beobachtete Tatsache. Der
//! Nullzähler sichert damit **nicht** einen unmöglichen Pfad ab (Zierrat),
//! sondern genau den einen Rest, der mit den vorhandenen Typen nicht
//! typisierbar ist: eine korrekt implementierte, aber falsch deklarierte
//! `Embedder::locality()`. Der Rest der Invariante — dass ein *aufrichtig*
//! deklarierter Remote-Embedder nie an `embed()` für `operator-only`-Text
//! gerät — ist strukturell durch den frühen `return Err(...)` in
//! [`build_visibility_bucket`] erzwungen, nicht bloß geprüft.
//!
//! # Nebenläufigkeit
//! [`build_index`] hält keinen Zustand zwischen Aufrufen; parallele Aufrufe
//! für unterschiedliche `(home, index_name)`-Paare sind sicher. Zwei
//! gleichzeitige Aufrufe für **dasselbe** `(home, index_name, visibility)`-
//! Tripel serialisieren sich über die `fs4`-Advisory-Lock, die
//! [`harw_lens_store::LensStore::put_chunk`]/`put_index` bereits halten.
//!
//! # Fehler
//! Siehe [`crate::SourceError`] für die vollständige Variantenliste.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use harw_lens_chunk::{chunk_markdown, chunk_rust};
use harw_lens_embed::{prepare_document, Embedder, EmbeddingDescriptor};
use harw_lens_index::FlatIndex;
use harw_lens_store::LensStore;
use harw_lens_types::{Chunk, ChunkDigest, IndexManifest, Locality, Metric};
use harw_types::ContentDigest;
use harw_observe::{
    Cardinality, MetricKey, MetricKind, NullCounter, NullSink, Unit,
};
use serde::{Deserialize, Serialize};

use crate::document::RawDocument;
use crate::error::{SourceError, SourceResult};
use crate::{CHUNKER_VERSION, CODE_RUST_INDEX, OPERATOR_ONLY_VISIBILITY};

/// Metrikschlüssel des Nullzählers [`LENS_REMOTE_EMBED_ON_OPERATOR_ONLY`].
const LENS_REMOTE_EMBED_ON_OPERATOR_ONLY_KEY: MetricKey = MetricKey {
    name: "lens_remote_embed_on_operator_only_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Der Nullzähler dieses Knotens (AW7-05): zählt, wie oft
/// [`build_visibility_bucket`] einen `operator-only`-Sichtbarkeits-Bucket
/// mit einem Embedder abgewiesen hat, dessen
/// [`harw_lens_embed::Embedder::locality`] [`Locality::Remote`] meldet.
/// Erwarteter Wert im Betrieb: **null**. Siehe den Moduldoku-Abschnitt „Der
/// Nullzähler `lens_remote_embed_on_operator_only`" für den Pfad, den dieser
/// Zähler beobachtet, und die ehrliche Einschränkung, was er (noch) nicht
/// beweisen kann.
pub static LENS_REMOTE_EMBED_ON_OPERATOR_ONLY: NullCounter = NullCounter::new(
    &LENS_REMOTE_EMBED_ON_OPERATOR_ONLY_KEY,
    "kein operator-only-Sichtbarkeits-Bucket wird je einem Embedder mit Locality::Remote zugewiesen",
);

/// Ergebnis eines Builds für genau einen Sichtbarkeits-Bucket.
///
/// # Description
/// Der Aufzählungscharakter (`embedded_count` vs. `reused_count`) macht
/// Inkrementalität nachweisbar, ohne dass ein Test den Embedder selbst
/// instrumentieren müsste — siehe aber
/// `test_build_index_second_build_over_unchanged_material_never_calls_embedder`
/// dafür, dass tatsächlich auch kein Aufruf stattfand, nicht nur, dass
/// keiner *nötig* gewesen wäre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexBuildReport {
    /// Name des gebauten Index (z. B. [`crate::DOCS_DESIGN_INDEX`]).
    pub index_name: String,
    /// Sichtbarkeits-Bucket, in den dieser Teil-Build geschrieben wurde.
    pub visibility: String,
    /// Gesamtzahl der Chunk-Instanzen in diesem Build (Duplikate zählen
    /// mehrfach).
    pub chunk_count: usize,
    /// Anzahl **einzigartiger** Chunk-Digests, die in diesem Build tatsächlich
    /// an den Embedder gegeben wurden.
    pub embedded_count: usize,
    /// Anzahl Chunk-Instanzen, deren Embedding aus dem Cache wiederverwendet
    /// wurde, statt neu berechnet zu werden.
    pub reused_count: usize,
}

/// Baut (oder aktualisiert) einen benannten Index aus rohen Dokumenten.
///
/// # Description
/// Gruppiert `documents` nach `RawDocument::visibility` und baut für jede
/// Gruppe einen eigenen, physisch getrennten [`FlatIndex`] unter
/// [`harw_home::paths::visibility_index_dir`] auf. Siehe die
/// Moduldokumentation für die Inkrementalitäts- und
/// Sichtbarkeitstrennungs-Mechanik im Detail. `descriptor.document_prefix`
/// wird über [`prepare_document`] auf jeden neu einzubettenden Chunk-Text
/// angewendet — nie am Aufrufort von Hand geschrieben.
///
/// # Arguments
/// - `home` (`&Path`): der Root-Space, unter dem
///   [`harw_home::paths::visibility_index_dir`] aufgelöst wird.
/// - `index_name` (`&str`): der Indexname (z. B.
///   [`crate::DOCS_DESIGN_INDEX`]/[`crate::KNOWLEDGE_PALACE_INDEX`]).
/// - `documents` (`&[RawDocument]`): die zu indizierenden Dokumente, aus
///   [`crate::collect_design_docs`]/[`crate::collect_palace_documents`] oder
///   einer eigenen Quelle.
/// - `model` (`&str`): Name/Kennung des Embedding-Modells, ins
///   [`IndexManifest`] und in den Embedding-Cache-Schlüssel übernommen.
/// - `locality` (`Locality`): wo `embedder` rechnet, ins [`IndexManifest`]
///   übernommen.
/// - `metric` (`Metric`): das Abstandsmaß, ins [`IndexManifest`] übernommen.
/// - `embedder` (`&dyn Embedder`): berechnet Vektoren für neue Chunks; wird
///   **nicht** aufgerufen, wenn kein Chunk dieser Sichtbarkeitsgruppe neu ist.
/// - `descriptor` (`&EmbeddingDescriptor`): liefert das Dokument-Präfix für
///   [`prepare_document`].
///
/// # Returns
/// Einen [`IndexBuildReport`] je Sichtbarkeits-Bucket, der in `documents`
/// vorkam, in aufsteigender Sortierung nach Bucket-Name.
///
/// # Errors
/// - [`SourceError::Home`]: `visibility` ist kein gültiger
///   Sichtbarkeits-Bucket-Name.
/// - [`SourceError::Store`]: ein Speicherfehler beim Ablegen von Chunks, dem
///   Index oder dem Embedding-Cache.
/// - [`SourceError::Index`]: ein Fehler beim Speichern des [`FlatIndex`].
/// - [`SourceError::Embed`]: der Embedder schlägt fehl.
/// - [`SourceError::EmbedderCountMismatch`]: der Embedder liefert eine andere
///   Vektoranzahl als angefragt.
/// - [`SourceError::MissingEmbedding`]: interner Konsistenzfehler.
/// - [`SourceError::Serde`]/[`SourceError::Io`]: beim (De-)Serialisieren bzw.
///   Lesen des Embedding-Caches.
/// - [`SourceError::OperatorOnlyRemoteEmbed`]: `embedder.locality()` meldet
///   [`Locality::Remote`], aber `documents` enthält einen
///   `operator-only`-Sichtbarkeits-Bucket (Knoten AW7-05, siehe die
///   Moduldoku).
///
/// # Examples
/// ```rust,no_run
/// use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
/// use harw_lens_types::{Locality, Metric};
/// use harw_lens_source::{build_index, collect_design_docs, DOCS_DESIGN_INDEX};
///
/// let home = tempfile::tempdir()?;
/// let docs_root = tempfile::tempdir()?;
/// std::fs::write(docs_root.path().join("intro.md"), "# Intro\n\nText.\n")?;
///
/// let documents = collect_design_docs(docs_root.path())?;
/// let descriptor = EmbeddingDescriptor {
///     document_prefix: "passage: ".to_owned(),
///     query_prefix: "query: ".to_owned(),
///     normalize: false,
/// };
/// let embedder = DeterministicEmbedder::new(16);
/// let reports = build_index(
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
/// assert_eq!(reports[0].embedded_count, 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[allow(clippy::too_many_arguments)]
pub fn build_index(
    home: &Path,
    index_name: &str,
    documents: &[RawDocument],
    model: &str,
    locality: Locality,
    metric: Metric,
    embedder: &dyn Embedder,
    descriptor: &EmbeddingDescriptor,
) -> SourceResult<Vec<IndexBuildReport>> {
    let mut by_visibility: HashMap<&str, Vec<&RawDocument>> = HashMap::new();
    for document in documents {
        by_visibility
            .entry(document.visibility.as_str())
            .or_default()
            .push(document);
    }

    let mut visibilities: Vec<&str> = by_visibility.keys().copied().collect();
    visibilities.sort_unstable();

    let mut reports = Vec::with_capacity(visibilities.len());
    for visibility in visibilities {
        let docs = &by_visibility[visibility];
        reports.push(build_visibility_bucket(
            home, index_name, visibility, docs, model, locality, metric, embedder, descriptor,
        )?);
    }
    Ok(reports)
}

/// Baut den [`FlatIndex`] genau eines Sichtbarkeits-Buckets. Siehe
/// [`build_index`] für die vollständige Dokumentation.
#[allow(clippy::too_many_arguments)]
fn build_visibility_bucket(
    home: &Path,
    index_name: &str,
    visibility: &str,
    documents: &[&RawDocument],
    model: &str,
    locality: Locality,
    metric: Metric,
    embedder: &dyn Embedder,
    descriptor: &EmbeddingDescriptor,
) -> SourceResult<IndexBuildReport> {
    // Fail-Closed, geprüft vor jeder anderen Arbeit an diesem Bucket:
    // `operator-only`-Sichtbarkeit darf nie an einen Embedder gehen, der den
    // Host verlässt. Siehe die Moduldoku, Abschnitt „Die Falle, die dieser
    // Knoten prüft", für die vollständige Begründung dieser Reihenfolge
    // (unabhängig davon, ob dieser konkrete Aufruf überhaupt neue Chunks zum
    // Einbetten hätte).
    if visibility == OPERATOR_ONLY_VISIBILITY && embedder.locality() == Locality::Remote {
        LENS_REMOTE_EMBED_ON_OPERATOR_ONLY.violated(&NullSink, &[]);
        return Err(SourceError::OperatorOnlyRemoteEmbed {
            index_name: index_name.to_owned(),
        });
    }

    let store_root = harw_home::paths::visibility_index_dir(home, visibility)?;
    let store = LensStore::open(&store_root)?;

    let mut chunks: Vec<Chunk> = Vec::new();
    for document in documents {
        chunks.extend(chunk_for_index(index_name, &document.source, &document.text));
    }

    for chunk in &chunks {
        if !store.has_chunk(&chunk.digest)? {
            store.put_chunk(chunk)?;
        }
    }

    let cache_name = embedding_cache_name(index_name);
    let mut cache = load_embedding_cache(&store, &cache_name, model, CHUNKER_VERSION)?;

    let mut newly_embedded: HashSet<ChunkDigest> = HashSet::new();
    let mut new_indices: Vec<usize> = Vec::new();
    let mut new_texts: Vec<String> = Vec::new();
    for (position, chunk) in chunks.iter().enumerate() {
        if cache.contains_key(&chunk.digest) || newly_embedded.contains(&chunk.digest) {
            continue;
        }
        newly_embedded.insert(chunk.digest);
        new_indices.push(position);
        new_texts.push(prepare_document(descriptor, &chunk.text));
    }

    if !new_texts.is_empty() {
        let vectors = embedder.embed(&new_texts)?;
        if vectors.len() != new_texts.len() {
            return Err(SourceError::EmbedderCountMismatch {
                expected: new_texts.len(),
                actual: vectors.len(),
            });
        }
        for (position, vector) in new_indices.into_iter().zip(vectors) {
            cache.insert(chunks[position].digest, vector);
        }
    }

    let source_set_digest = compute_source_set_digest(&chunks);
    let manifest = IndexManifest {
        model: model.to_owned(),
        locality,
        chunker_version: CHUNKER_VERSION,
        visibility: visibility.to_owned(),
        metric,
        source_set_digest,
    };

    let mut entries: Vec<(Chunk, Vec<f32>)> = Vec::with_capacity(chunks.len());
    for chunk in &chunks {
        let embedding = cache
            .get(&chunk.digest)
            .cloned()
            .ok_or_else(|| SourceError::MissingEmbedding {
                digest: chunk.digest.0.to_string(),
            })?;
        entries.push((chunk.clone(), embedding));
    }

    let index = FlatIndex::build(manifest.clone(), entries);
    index.save(&store, index_name)?;
    save_embedding_cache(&store, &cache_name, &manifest, &cache)?;

    let embedded_count = newly_embedded.len();
    let reused_instances = chunks
        .iter()
        .filter(|chunk| !newly_embedded.contains(&chunk.digest))
        .count();

    Ok(IndexBuildReport {
        index_name: index_name.to_owned(),
        visibility: visibility.to_owned(),
        chunk_count: chunks.len(),
        embedded_count,
        reused_count: reused_instances,
    })
}

/// Wählt die Zerlegungsstrategie anhand des Indexnamens. Siehe die
/// Moduldoku, Abschnitt „Welche Zerlegungsstrategie ein Index bekommt", für
/// die Begründung, warum das über `index_name` statt über einen
/// zusätzlichen Parameter entschieden wird.
///
/// # Returns
/// [`harw_lens_chunk::chunk_rust`] für [`crate::CODE_RUST_INDEX`], sonst
/// [`harw_lens_chunk::chunk_markdown`].
fn chunk_for_index(
    index_name: &str,
    source: &harw_lens_types::SourceRef,
    text: &str,
) -> Vec<Chunk> {
    if index_name == CODE_RUST_INDEX {
        chunk_rust(source, text)
    } else {
        chunk_markdown(source, text)
    }
}

/// Name, unter dem der Digest-zu-Vektor-Cache eines Index in `LensStore`
/// abgelegt wird. `.`/`-` sind gültige Zeichen für einen `LensStore`-Indexnamen
/// (siehe `harw-lens-store`s `validate_index_name`).
fn embedding_cache_name(index_name: &str) -> String {
    format!("{index_name}.embedding-cache")
}

/// Ein einzelner Digest-zu-Vektor-Eintrag des Embedding-Caches.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Der Chunk-Digest, für den `embedding` berechnet wurde.
    digest: ChunkDigest,
    /// Der zwischengespeicherte Vektor.
    embedding: Vec<f32>,
}

/// Das Persistenzformat des Embedding-Caches: eine flache Liste von
/// Digest/Vektor-Paaren.
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    /// Alle zwischengespeicherten Einträge.
    entries: Vec<CacheEntry>,
}

/// Lädt den Embedding-Cache eines Index, sofern er zu `model`/`chunker_version`
/// passt.
///
/// # Description
/// Ein fehlender Cache (erster Build) liefert eine leere Map, kein Fehler.
/// Ein Cache, dessen gespeichertes Manifest von `model`/`chunker_version`
/// abweicht, gilt als veraltet und wird ebenfalls als leere Map behandelt —
/// ein Modell- oder Chunker-Wechsel embettet alles neu, statt Vektoren aus
/// unterschiedlichen Räumen in einem Index zu mischen.
fn load_embedding_cache(
    store: &LensStore,
    cache_name: &str,
    model: &str,
    chunker_version: u32,
) -> SourceResult<HashMap<ChunkDigest, Vec<f32>>> {
    let Some(manifest) = store.get_index_manifest(cache_name)? else {
        return Ok(HashMap::new());
    };
    if manifest.model != model || manifest.chunker_version != chunker_version {
        return Ok(HashMap::new());
    }
    let Some(data) = store.read_index_data(cache_name)? else {
        return Ok(HashMap::new());
    };
    let file: CacheFile = serde_json::from_slice(&data)?;
    Ok(file
        .entries
        .into_iter()
        .map(|entry| (entry.digest, entry.embedding))
        .collect())
}

/// Persistiert den Embedding-Cache eines Index unter demselben Manifest wie
/// der zugehörige [`FlatIndex`].
fn save_embedding_cache(
    store: &LensStore,
    cache_name: &str,
    manifest: &IndexManifest,
    cache: &HashMap<ChunkDigest, Vec<f32>>,
) -> SourceResult<()> {
    let file = CacheFile {
        entries: cache
            .iter()
            .map(|(digest, embedding)| CacheEntry {
                digest: *digest,
                embedding: embedding.to_vec(),
            })
            .collect(),
    };
    let data = serde_json::to_vec(&file)?;
    store.put_index(cache_name, manifest, &data)?;
    Ok(())
}

/// Berechnet einen deterministischen Digest über die Menge der indizierten
/// Chunks, unabhängig von ihrer Reihenfolge in `chunks`.
///
/// # Description
/// Sortiert die rohen Digest-Bytes aller Chunks, verkettet sie und hasht das
/// Ergebnis erneut über [`ContentDigest::of`]. Zwei Builds mit derselben
/// Chunk-*Menge* (unabhängig von der Iterationsreihenfolge) liefern denselben
/// `source_set_digest`.
fn compute_source_set_digest(chunks: &[Chunk]) -> ContentDigest {
    let mut digests: Vec<[u8; 32]> = chunks.iter().map(|chunk| *chunk.digest.0.as_bytes()).collect();
    digests.sort_unstable();
    let mut buffer = Vec::with_capacity(digests.len() * 32);
    for digest in digests {
        buffer.extend_from_slice(&digest);
    }
    ContentDigest::of(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_embed::EmbeddingDescriptor;
    use harw_lens_index::{Query, VectorIndex};
    use harw_lens_types::SourceRef;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Zählt jeden Aufruf von `embed`, um Inkrementalität nachweisbar zu
    /// machen: "der Embedder wurde N-mal aufgerufen" ist sonst nur eine
    /// Behauptung.
    struct CountingEmbedder {
        inner: harw_lens_embed::DeterministicEmbedder,
        call_count: AtomicUsize,
        seen_texts: Mutex<Vec<String>>,
    }

    impl CountingEmbedder {
        fn new(dimensions: usize) -> Self {
            Self {
                inner: harw_lens_embed::DeterministicEmbedder::new(dimensions),
                call_count: AtomicUsize::new(0),
                seen_texts: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }

        fn texts_seen(&self) -> Vec<String> {
            self.seen_texts.lock().expect("lock poisoned").clone()
        }
    }

    impl Embedder for CountingEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, harw_lens_embed::EmbedError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.seen_texts
                .lock()
                .expect("lock poisoned")
                .extend(texts.iter().cloned());
            self.inner.embed(texts)
        }

        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        /// Delegiert an `inner` ([`harw_lens_embed::DeterministicEmbedder`],
        /// stets `Local`) statt sich auf den `Embedder::locality`-Default
        /// (`Remote`) zu verlassen -- dieser Test-Double steht auch in
        /// `test_build_index_separates_operator_only_visibility_from_workspace_store`
        /// für einen `operator-only`-Bucket im Einsatz und muss dort
        /// zugelassen bleiben.
        fn locality(&self) -> Locality {
            self.inner.locality()
        }
    }

    /// Meldet [`Locality::Remote`] und zählt jeden `embed`-Aufruf -- das
    /// Werkzeug, mit dem die Tests unten beweisen, dass ein
    /// `operator-only`-Bucket `embed()` auf einem entfernten Embedder
    /// **nie** erreicht (statt sich nur auf die Fehlervariante zu
    /// verlassen).
    struct RemoteCountingEmbedder {
        inner: harw_lens_embed::DeterministicEmbedder,
        call_count: AtomicUsize,
    }

    impl RemoteCountingEmbedder {
        fn new(dimensions: usize) -> Self {
            Self {
                inner: harw_lens_embed::DeterministicEmbedder::new(dimensions),
                call_count: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl Embedder for RemoteCountingEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, harw_lens_embed::EmbedError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.inner.embed(texts)
        }

        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        fn locality(&self) -> Locality {
            Locality::Remote
        }
    }

    fn descriptor() -> EmbeddingDescriptor {
        EmbeddingDescriptor {
            document_prefix: "passage: ".to_owned(),
            query_prefix: "query: ".to_owned(),
            normalize: false,
        }
    }

    fn one_document(text: &str) -> RawDocument {
        RawDocument {
            source: SourceRef::File {
                path: "doc.md".to_owned(),
            },
            text: text.to_owned(),
            visibility: crate::DEFAULT_VISIBILITY.to_owned(),
        }
    }

    #[test]
    fn test_build_index_first_build_embeds_every_chunk() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let documents = vec![one_document("# Title\n\nBody text.\n")];

        let reports = build_index(
            home.path(),
            "docs.design",
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("builds");

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].embedded_count, reports[0].chunk_count);
        assert_eq!(reports[0].reused_count, 0);
        assert_eq!(embedder.calls(), 1);
    }

    #[test]
    fn test_build_index_second_build_over_unchanged_material_never_calls_embedder() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let documents = vec![one_document("# Title\n\nBody text.\n")];

        build_index(
            home.path(),
            "docs.design",
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("first build");
        assert_eq!(embedder.calls(), 1);

        let reports = build_index(
            home.path(),
            "docs.design",
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("second build");

        assert_eq!(embedder.calls(), 1, "second build must not call the embedder again");
        assert_eq!(reports[0].embedded_count, 0);
        assert_eq!(reports[0].reused_count, reports[0].chunk_count);
    }

    #[test]
    fn test_build_index_changing_one_document_only_reembeds_its_chunks() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let unchanged = RawDocument {
            source: SourceRef::File {
                path: "unchanged.md".to_owned(),
            },
            text: "# Unchanged\n\nStays the same.\n".to_owned(),
            visibility: crate::DEFAULT_VISIBILITY.to_owned(),
        };
        let mut changing = RawDocument {
            source: SourceRef::File {
                path: "changing.md".to_owned(),
            },
            text: "# Changing\n\nOriginal body.\n".to_owned(),
            visibility: crate::DEFAULT_VISIBILITY.to_owned(),
        };

        build_index(
            home.path(),
            "docs.design",
            &[unchanged.clone(), changing.clone()],
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("first build");
        let first_call_count = embedder.calls();
        assert!(first_call_count >= 1);

        changing.text = "# Changing\n\nA different body now.\n".to_owned();
        let reports = build_index(
            home.path(),
            "docs.design",
            &[unchanged.clone(), changing.clone()],
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("second build");

        assert_eq!(
            embedder.calls(),
            first_call_count + 1,
            "exactly one more batch call for the changed document's chunks"
        );
        // Every freshly embedded text carries the changed body's content and
        // the document prefix — never the unchanged document's text.
        let seen = embedder.texts_seen();
        let last_batch = &seen[seen.len() - reports.last().expect("has report").embedded_count..];
        for text in last_batch {
            assert!(text.starts_with("passage: "));
            assert!(text.contains("A different body now."));
            assert!(!text.contains("Stays the same."));
        }
    }

    #[test]
    fn test_build_index_applies_document_prefix_before_embedding() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let documents = vec![one_document("plain body text")];

        build_index(
            home.path(),
            "docs.design",
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("builds");

        let seen = embedder.texts_seen();
        assert!(seen.iter().all(|text| text.starts_with("passage: ")));
    }

    #[test]
    fn test_build_index_is_queryable_and_hits_carry_the_right_source_ref() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let documents = vec![one_document("# Title\n\nBody text.\n")];

        build_index(
            home.path(),
            "docs.design",
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("builds");

        let store_root = harw_home::paths::visibility_index_dir(home.path(), "workspace")
            .expect("valid visibility name");
        let store = LensStore::open(&store_root).expect("opens store");
        let index = FlatIndex::load(&store, "docs.design").expect("loads index");

        let embedding = embedder
            .embed(&[harw_lens_embed::prepare_document(&descriptor(), "# Title\n\nBody text.\n")])
            .expect("embeds")
            .pop()
            .expect("one vector");
        let query = Query {
            embedding: Some(embedding),
            text: None,
            manifest: index.manifest().clone(),
        };
        let hits = index.search(&query, 10).expect("search succeeds");
        assert!(!hits.is_empty());
        assert_eq!(
            hits[0].chunk.source,
            SourceRef::File {
                path: "doc.md".to_owned()
            }
        );
    }

    #[test]
    fn test_build_index_separates_operator_only_visibility_from_workspace_store() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let workspace_doc = RawDocument {
            source: SourceRef::Artifact {
                id: "palace/normal".to_owned(),
            },
            text: "ordinary palace content".to_owned(),
            visibility: crate::DEFAULT_VISIBILITY.to_owned(),
        };
        let operator_doc = RawDocument {
            source: SourceRef::Artifact {
                id: "palace/secret".to_owned(),
            },
            text: "operator only secret content".to_owned(),
            visibility: crate::OPERATOR_ONLY_VISIBILITY.to_owned(),
        };

        let reports = build_index(
            home.path(),
            "knowledge.palace",
            &[workspace_doc, operator_doc],
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("builds");
        assert_eq!(reports.len(), 2);

        let workspace_root = harw_home::paths::visibility_index_dir(home.path(), "workspace")
            .expect("valid visibility name");
        let operator_root =
            harw_home::paths::visibility_index_dir(home.path(), "operator-only")
                .expect("valid visibility name");
        assert_ne!(workspace_root, operator_root);

        let workspace_store = LensStore::open(&workspace_root).expect("opens workspace store");
        let workspace_index =
            FlatIndex::load(&workspace_store, "knowledge.palace").expect("loads workspace index");

        // The most important security test of this node: the secret content's
        // chunk must not exist at all in the physically separate workspace
        // store -- not merely be filtered out of a shared one.
        let secret_chunks = harw_lens_chunk::chunk_markdown(
            &SourceRef::Artifact {
                id: "palace/secret".to_owned(),
            },
            "operator only secret content",
        );
        for chunk in &secret_chunks {
            assert!(
                !workspace_store.has_chunk(&chunk.digest).expect("checks"),
                "operator-only chunk must not exist in the workspace-visibility store"
            );
        }
        assert!(workspace_index
            .search(
                &Query {
                    embedding: Some(vec![0.0; 8]),
                    text: None,
                    manifest: workspace_index.manifest().clone(),
                },
                10,
            )
            .expect("search succeeds")
            .iter()
            .all(|hit| hit.chunk.source
                != SourceRef::Artifact {
                    id: "palace/secret".to_owned()
                }));

        let operator_store = LensStore::open(&operator_root).expect("opens operator store");
        let operator_index =
            FlatIndex::load(&operator_store, "knowledge.palace").expect("loads operator index");
        assert!(!operator_index.is_empty());
    }

    fn operator_only_document(text: &str, id: &str) -> RawDocument {
        RawDocument {
            source: SourceRef::Artifact { id: id.to_owned() },
            text: text.to_owned(),
            visibility: crate::OPERATOR_ONLY_VISIBILITY.to_owned(),
        }
    }

    /// Der wichtigste Test dieses Knotens: `operator-only`-Material erreicht
    /// die entfernte Schicht nicht. Beweist zwei Dinge, nicht nur eines --
    /// dass `build_index` einen Fehler zurückgibt, UND dass
    /// `Embedder::embed` dabei kein einziges Mal aufgerufen wurde (sonst
    /// bewiese der Test nur "es endet mit einem Fehler", nicht "der Text
    /// wurde nie verschickt").
    #[test]
    fn test_build_index_operator_only_bucket_with_remote_embedder_is_rejected_before_embedding() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = RemoteCountingEmbedder::new(8);
        let documents = vec![operator_only_document(
            "operator only secret content",
            "palace/secret",
        )];

        let before = LENS_REMOTE_EMBED_ON_OPERATOR_ONLY.count();

        let result = build_index(
            home.path(),
            "knowledge.palace",
            &documents,
            "test-model",
            Locality::Remote,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        );

        match &result {
            Err(SourceError::OperatorOnlyRemoteEmbed { index_name }) => {
                assert_eq!(index_name, "knowledge.palace");
            }
            other => panic!("expected SourceError::OperatorOnlyRemoteEmbed, got {other:?}"),
        }
        assert_eq!(
            embedder.calls(),
            0,
            "operator-only text must never reach Embedder::embed on a remote embedder"
        );
        assert!(
            LENS_REMOTE_EMBED_ON_OPERATOR_ONLY.count() > before,
            "the null counter must be driven past zero by an actual violation"
        );
    }

    /// Gegenprobe zum obigen Test: derselbe Remote-Embedder bleibt für einen
    /// gewöhnlichen `workspace`-Bucket zugelassen -- der Guard darf nicht
    /// überbreit greifen.
    #[test]
    fn test_build_index_workspace_bucket_with_remote_embedder_still_succeeds() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = RemoteCountingEmbedder::new(8);
        let documents = vec![one_document("# Title\n\nOrdinary body.\n")];

        let reports = build_index(
            home.path(),
            "docs.design",
            &documents,
            "test-model",
            Locality::Remote,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("workspace visibility is not subject to the operator-only guard");

        assert_eq!(reports.len(), 1);
        assert_eq!(embedder.calls(), 1);
    }

    /// [`crate::CODE_RUST_INDEX`] wird mit `chunk_rust` zerlegt, nicht mit
    /// `chunk_markdown` -- nachgewiesen, indem ein Rust-Quelltext, dessen
    /// Markdown-Zerlegung anders ausfiele (kein `#`-Überschriftenzeichen),
    /// trotzdem an Element-Grenzen (`fn`) zerlegt wird.
    #[test]
    fn test_build_index_code_rust_index_uses_chunk_rust_not_chunk_markdown() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let source_text = "fn a() {}\nfn b() {}\n";
        let documents = vec![RawDocument {
            source: SourceRef::File {
                path: "src/lib.rs".to_owned(),
            },
            text: source_text.to_owned(),
            visibility: crate::DEFAULT_VISIBILITY.to_owned(),
        }];

        build_index(
            home.path(),
            crate::CODE_RUST_INDEX,
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("builds");

        let expected = harw_lens_chunk::chunk_rust(
            &SourceRef::File {
                path: "src/lib.rs".to_owned(),
            },
            source_text,
        );
        assert_eq!(expected.len(), 2, "chunk_rust splits at fn boundaries");
        let seen = embedder.texts_seen();
        assert!(seen.iter().any(|text| text.contains("fn a() {}")));
        assert!(seen.iter().any(|text| text.contains("fn b() {}")));
    }

    /// Determinismus für [`crate::CODE_RUST_INDEX`]: derselbe Quelltext
    /// erzeugt beim zweiten Build dieselben Digests und ruft den Embedder
    /// kein einziges Mal erneut auf -- dieselbe Zusicherung, die
    /// `test_build_index_second_build_over_unchanged_material_never_calls_embedder`
    /// für `docs.design` belegt, hier für die zweite Zerlegungsstrategie.
    #[test]
    fn test_build_index_code_rust_index_second_build_never_calls_embedder() {
        let home = tempfile::tempdir().expect("tempdir");
        let embedder = CountingEmbedder::new(8);
        let documents = vec![RawDocument {
            source: SourceRef::File {
                path: "src/lib.rs".to_owned(),
            },
            text: "fn a() {}\n".to_owned(),
            visibility: crate::DEFAULT_VISIBILITY.to_owned(),
        }];

        build_index(
            home.path(),
            crate::CODE_RUST_INDEX,
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("first build");
        assert_eq!(embedder.calls(), 1);

        let reports = build_index(
            home.path(),
            crate::CODE_RUST_INDEX,
            &documents,
            "test-model",
            Locality::Local,
            Metric::Cosine,
            &embedder,
            &descriptor(),
        )
        .expect("second build");

        assert_eq!(embedder.calls(), 1, "second build must not call the embedder again");
        assert_eq!(reports[0].embedded_count, 0);
    }
}
