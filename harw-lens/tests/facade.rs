//! Integrationstests der Fassade `harw-lens` (Knoten AW5-10).
//!
//! Deckt die vier im Auftrag verlangten Szenarien ab:
//! 1. ein vollständiger Durchlauf über die Fassade (bauen, fragen, Treffer
//!    bekommen);
//! 2. ein Selektor außerhalb des Lesebereichs liefert einen Fehler, keine
//!    leere Liste;
//! 3. dieser Fehler bleibt von einem anderen Fehler unterscheidbar, auch
//!    nach dem Wickeln in [`harw_lens::LensError`];
//! 4. [`harw_lens::ask`] liefert exakt dieselben Treffer wie der direkte Weg
//!    über `harw_lens_query::query_scoped` — Beleg dafür, dass die Fassade
//!    selbst nichts berechnet.

use harw_lens::{
    ask, build, index_status, IndexSelector, LensError, QueryError, QueryProvenance, ReadScope,
    CHUNKER_VERSION, DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX, OPERATOR_ONLY_VISIBILITY,
};
use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
use harw_lens_types::{CollapsePolicy, EdgeIndex, Locality, Metric, SourceRef};

fn descriptor() -> EmbeddingDescriptor {
    EmbeddingDescriptor {
        document_prefix: "passage: ".to_owned(),
        query_prefix: "query: ".to_owned(),
        normalize: false,
    }
}

/// Die zu jedem `build(...)`-Aufruf in dieser Datei passende Provenienz --
/// dasselbe Modell (`"test-model"`), mit dem hier durchgängig gebaut wird.
fn provenance() -> QueryProvenance {
    QueryProvenance {
        model: "test-model".to_owned(),
        chunker_version: CHUNKER_VERSION,
    }
}

fn document(text: &str, path: &str, visibility: &str) -> harw_lens::RawDocument {
    harw_lens::RawDocument {
        source: SourceRef::File {
            path: path.to_owned(),
        },
        text: text.to_owned(),
        visibility: visibility.to_owned(),
    }
}

#[test]
fn test_build_then_ask_round_trip_finds_hits() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![document(
        "Harwness Fassaden binden zehn Crates unter einem Namen.",
        "intro.md",
        DEFAULT_VISIBILITY,
    )];

    let reports = build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .expect("build succeeds");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].visibility, DEFAULT_VISIBILITY);
    assert_eq!(reports[0].chunk_count, 1);
    assert_eq!(reports[0].embedded_count, 1);

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let hits = ask(
        home.path(),
        &selector,
        &scope,
        "Fassaden binden Crates",
        &embedder,
        &descriptor,
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("ask succeeds");

    assert_eq!(hits.len(), 1);
    assert!(hits[0].chunk.text.contains("Fassaden"));
}

#[test]
fn test_ask_outside_read_scope_returns_error_not_empty_list() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![document(
        "Nur fuer Operatoren sichtbarer Inhalt.",
        "secret.md",
        OPERATOR_ONLY_VISIBILITY,
    )];

    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .expect("build succeeds");

    // Der Selektor fragt exakt den Index/die Sichtbarkeit an, unter der
    // gerade gebaut wurde -- nur der Lesebereich des Aufrufers gibt sie
    // nicht frei.
    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, OPERATOR_ONLY_VISIBILITY);
    let scope = ReadScope::single(DEFAULT_VISIBILITY);

    let result = ask(
        home.path(),
        &selector,
        &scope,
        "Operatoren",
        &embedder,
        &descriptor,
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    );

    // Der wichtigste Fall: ein Fehler, niemals `Ok(vec![])`.
    let err = result.expect_err("selector outside read scope must be rejected");
    assert!(matches!(
        err,
        LensError::Query(QueryError::IndexNotVisible {
            ref index_name,
            ref visibility,
        }) if index_name == DOCS_DESIGN_INDEX && visibility == OPERATOR_ONLY_VISIBILITY
    ));
}

#[test]
fn test_index_not_visible_is_distinguishable_from_missing_index_error() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();

    // Fall A: Sichtbarkeit außerhalb des Lesebereichs -- kein Build nötig,
    // `resolve_index` scheitert bereits vor jedem Dateisystemzugriff.
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let out_of_scope_selector = IndexSelector::new(DOCS_DESIGN_INDEX, OPERATOR_ONLY_VISIBILITY);
    let out_of_scope_err = ask(
        home.path(),
        &out_of_scope_selector,
        &scope,
        "irrelevant",
        &embedder,
        &descriptor,
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect_err("out-of-scope selector must be rejected");

    // Fall B: Sichtbarkeit innerhalb des Lesebereichs, aber kein Index
    // dieses Namens existiert -- ein anderer Fehler als Fall A.
    let missing_index_selector = IndexSelector::new("does-not-exist", DEFAULT_VISIBILITY);
    let missing_index_err = ask(
        home.path(),
        &missing_index_selector,
        &scope,
        "irrelevant",
        &embedder,
        &descriptor,
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect_err("missing index must be rejected");

    assert!(matches!(
        out_of_scope_err,
        LensError::Query(QueryError::IndexNotVisible { .. })
    ));
    assert!(matches!(
        missing_index_err,
        LensError::Query(QueryError::Index(_))
    ));
    // Nach dem Wickeln in `LensError` bleiben beide Varianten getrennt --
    // keine der beiden `matches!`-Prüfungen oben würde für den jeweils
    // anderen Fehler zutreffen.
}

#[test]
fn test_ask_computes_nothing_itself_same_hits_as_direct_query_scoped_call() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![
        document("Erster Chunk ueber Katzen.", "a.md", DEFAULT_VISIBILITY),
        document("Zweiter Chunk ueber Hunde.", "b.md", DEFAULT_VISIBILITY),
    ];

    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .expect("build succeeds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let edges = EdgeIndex::default();

    let via_facade = ask(
        home.path(),
        &selector,
        &scope,
        "Katzen und Hunde",
        &embedder,
        &descriptor,
        &provenance(),
        &edges,
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("ask succeeds");

    let via_direct_query_path = harw_lens_query::query_scoped(
        home.path(),
        &selector,
        &scope,
        "Katzen und Hunde",
        &embedder,
        &descriptor,
        &provenance(),
        &edges,
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("direct query_scoped call succeeds");

    assert!(!via_facade.is_empty());
    assert_eq!(via_facade, via_direct_query_path);
}

/// Der zentrale Beleg dieses Knotens: die Manifest-Prüfung in
/// `harw-lens-index` ist über die Fassade tatsächlich auslösbar. Ein mit
/// `"model-a"` gebauter Index lehnt eine Abfrage ab, die mit `"model-b"`
/// eingebettet wurde -- kein leeres Ergebnis, kein Treffer, sondern ein
/// Fehler, der auch nach dem Wickeln in `LensError` als
/// `QueryError::Index(_)` unterscheidbar bleibt, nicht auf eine
/// Zeichenkette zusammengefaltet.
#[test]
fn test_ask_rejects_a_query_embedded_with_a_different_model_than_the_index() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![document(
        "Harwness Fassaden binden zehn Crates unter einem Namen.",
        "intro.md",
        DEFAULT_VISIBILITY,
    )];

    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "model-a",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .expect("build succeeds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let mismatched_provenance = QueryProvenance {
        model: "model-b".to_owned(),
        chunker_version: CHUNKER_VERSION,
    };

    let result = ask(
        home.path(),
        &selector,
        &scope,
        "Fassaden binden Crates",
        &embedder,
        &descriptor,
        &mismatched_provenance,
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    );

    let err = result.expect_err(
        "a query embedded with a different model than the index must be rejected, never silently answered",
    );
    assert!(matches!(err, LensError::Query(QueryError::Index(_))));
}

/// Gegentest zum vorherigen: passende Provenienz liefert Treffer. Ohne
/// diesen Test bewiese der vorherige nur, dass irgendetwas an dem Aufruf
/// fehlschlägt, nicht dass die Prüfung modellspezifisch ist.
#[test]
fn test_ask_accepts_a_query_embedded_with_the_same_model_as_the_index() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![document(
        "Harwness Fassaden binden zehn Crates unter einem Namen.",
        "intro.md",
        DEFAULT_VISIBILITY,
    )];

    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "model-a",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .expect("build succeeds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let matching_provenance = QueryProvenance {
        model: "model-a".to_owned(),
        chunker_version: CHUNKER_VERSION,
    };

    let hits = ask(
        home.path(),
        &selector,
        &scope,
        "Fassaden binden Crates",
        &embedder,
        &descriptor,
        &matching_provenance,
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("provenance matching the index's model must be accepted");

    assert!(!hits.is_empty());
}

/// Derselbe Fehlertyp, ein anderer Grund: eine abweichende
/// Zerlegungsfassung verschiebt die Chunk-Grenzen, nicht den Vektorraum --
/// die Prüfung muss trotzdem über die Fassade greifen.
#[test]
fn test_ask_rejects_a_query_embedded_with_a_different_chunker_version_than_the_index() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![document(
        "Harwness Fassaden binden zehn Crates unter einem Namen.",
        "intro.md",
        DEFAULT_VISIBILITY,
    )];

    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .expect("build succeeds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let mismatched_provenance = QueryProvenance {
        model: "test-model".to_owned(),
        chunker_version: CHUNKER_VERSION + 1,
    };

    let err = ask(
        home.path(),
        &selector,
        &scope,
        "Fassaden binden Crates",
        &embedder,
        &descriptor,
        &mismatched_provenance,
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect_err("a query embedded against a different chunker version must be rejected");

    assert!(matches!(err, LensError::Query(QueryError::Index(_))));
}

/// [`index_status`] ist die im `crate`-`//!`-Block angekündigte
/// Introspektionsfunktion: `None` vor jedem `build`, `Some(IndexStatus)`
/// danach, mit `chunk_count`/`dimension` aus den tatsächlich gespeicherten
/// Einträgen statt nur dem Manifest.
#[test]
fn test_index_status_reflects_build_state_through_the_facade() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();

    let before = index_status(home.path(), DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY)
        .expect("no error before any build");
    assert_eq!(before, None);

    let documents = vec![document(
        "Harwness Fassaden binden zehn Crates unter einem Namen.",
        "intro.md",
        DEFAULT_VISIBILITY,
    )];
    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .expect("build succeeds");

    let after = index_status(home.path(), DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY)
        .expect("reads")
        .expect("index exists after build");
    assert_eq!(after.index_name, DOCS_DESIGN_INDEX);
    assert_eq!(after.visibility, DEFAULT_VISIBILITY);
    assert_eq!(after.model, "test-model");
    assert_eq!(after.locality, Locality::Local);
    assert_eq!(after.chunker_version, CHUNKER_VERSION);
    assert_eq!(after.dimension, Some(16));
    assert_eq!(after.chunk_count, 1);
    assert!(after.modified.is_some());
}
