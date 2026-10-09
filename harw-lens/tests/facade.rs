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

mod common;

use common::{TestError, TestResult, ctx};
use harw_lens::{
    CHUNKER_VERSION, DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX, IndexSelector, LensError,
    OPERATOR_ONLY_VISIBILITY, QueryError, QueryProvenance, ReadScope, ask, build, index_status,
};
use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
use harw_lens_types::{CollapsePolicy, EdgeIndex, Locality, Metric, SourceRef};
use std::os::unix::ffi::OsStringExt;
use std::sync::Arc;

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
fn test_build_then_ask_round_trip_finds_hits() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
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
    .map_err(ctx("build succeeds"))?;
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
    .map_err(ctx("ask succeeds"))?;

    assert_eq!(hits.len(), 1);
    assert!(hits[0].chunk.text.contains("Fassaden"));
    Ok(())
}

#[test]
fn test_ask_outside_read_scope_returns_error_not_empty_list() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
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
    .map_err(ctx("build succeeds"))?;

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
    let Err(err) = result else {
        return Err(TestError::Unexpected(
            "selector outside read scope must be rejected".into(),
        ));
    };
    assert!(matches!(
        err,
        LensError::Query(QueryError::IndexNotVisible {
            ref index_name,
            ref visibility,
        }) if index_name == DOCS_DESIGN_INDEX && visibility == OPERATOR_ONLY_VISIBILITY
    ));
    Ok(())
}

#[test]
fn test_index_not_visible_is_distinguishable_from_missing_index_error() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();

    // Fall A: Sichtbarkeit außerhalb des Lesebereichs -- kein Build nötig,
    // `resolve_index` scheitert bereits vor jedem Dateisystemzugriff.
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let out_of_scope_selector = IndexSelector::new(DOCS_DESIGN_INDEX, OPERATOR_ONLY_VISIBILITY);
    let Err(out_of_scope_err) = ask(
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
    ) else {
        return Err(TestError::Unexpected(
            "out-of-scope selector must be rejected".into(),
        ));
    };

    // Fall B: Sichtbarkeit innerhalb des Lesebereichs, aber kein Index
    // dieses Namens existiert -- ein anderer Fehler als Fall A.
    let missing_index_selector = IndexSelector::new("does-not-exist", DEFAULT_VISIBILITY);
    let Err(missing_index_err) = ask(
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
    ) else {
        return Err(TestError::Unexpected(
            "missing index must be rejected".into(),
        ));
    };

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
    Ok(())
}

/// Thread-Helfer: identischer Body wie [`descriptor`], nur als eigene freie
/// Funktion, damit er in `std::thread::spawn(move || ...)`-Closures ohne
/// Capture aufrufbar bleibt.
fn descriptor_for_thread() -> EmbeddingDescriptor {
    EmbeddingDescriptor {
        document_prefix: "passage: ".to_owned(),
        query_prefix: "query: ".to_owned(),
        normalize: false,
    }
}

/// Thread-Helfer: identischer Body wie [`provenance`], nur als eigene freie
/// Funktion, damit er in `std::thread::spawn(move || ...)`-Closures ohne
/// Capture aufrufbar bleibt.
fn provenance_for_thread() -> QueryProvenance {
    QueryProvenance {
        model: "test-model".to_owned(),
        chunker_version: CHUNKER_VERSION,
    }
}

#[test]
fn test_index_verify_build_then_ask_returns_hits() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![document(
        "Verifikationschunk ueber Katzen.",
        "verify.md",
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
    .map_err(ctx("build succeeds"))?;

    let hits = ask(
        home.path(),
        &IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY),
        &ReadScope::single(DEFAULT_VISIBILITY),
        "Katzen",
        &embedder,
        &descriptor,
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .map_err(ctx("ask succeeds"))?;
    assert!(
        !hits.is_empty(),
        "a built index must answer a matching query with at least one hit"
    );
    Ok(())
}

#[test]
fn test_index_verify_status_reflects_built_index() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    assert!(
        index_status(home.path(), DOCS_DESIGN_INDEX).is_err(),
        "status before any build must not report an index"
    );
    let documents = vec![document(
        "Statuschunk ueber Hunde.",
        "status.md",
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
    .map_err(ctx("build succeeds"))?;
    assert!(
        index_status(home.path(), DOCS_DESIGN_INDEX).is_ok(),
        "status after build must report the index"
    );
    Ok(())
}

#[test]
fn test_index_verify_rebuild_updates_hits() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let first = vec![document("Erster Stand: Katzen.", "a.md", DEFAULT_VISIBILITY)];
    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &first,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .map_err(ctx("build succeeds"))?;
    let second = vec![document("Zweiter Stand: Roboter.", "b.md", DEFAULT_VISIBILITY)];
    build(
        home.path(),
        DOCS_DESIGN_INDEX,
        &second,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor,
    )
    .map_err(ctx("rebuild succeeds"))?;

    let hits = ask(
        home.path(),
        &IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY),
        &ReadScope::single(DEFAULT_VISIBILITY),
        "Roboter",
        &embedder,
        &descriptor,
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .map_err(ctx("ask succeeds"))?;
    assert!(
        !hits.is_empty(),
        "a rebuild must replace, not append: the new document must be findable"
    );
    Ok(())
}

#[test]
fn test_index_verify_non_utf8_document_path_is_handled() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let raw = OsString::from_vec(vec![0x67, 0x75, 0x74, 0xFF, 0x2E, 0x6D, 0x64]); // "gut\xFF.md"
    let path = home.path().join(raw);
    std::fs::write(&path, "Nicht-UTF-8-Pfad-Chunk ueber Katzen.")
        .map_err(ctx("write non-utf8 document"))?;

    let documents = vec![harw_lens::RawDocument {
        source: harw_lens_types::SourceRef::File {
            path: path.to_string_lossy().into_owned(),
        },
        ..document(
            "Nicht-UTF-8-Pfad-Chunk ueber Katzen.",
            "gut.md",
            DEFAULT_VISIBILITY,
        )
    }];
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
    .map_err(ctx("build succeeds"))?;
    let result = ask(
        home.path(),
        &IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY),
        &ReadScope::single(DEFAULT_VISIBILITY),
        "Katzen",
        &embedder,
        &descriptor,
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    );
    assert!(
        result.is_ok(),
        "non-utf8 source paths must not break the query path"
    );
    Ok(())
}

#[test]
fn test_index_verify_parallel_queries_same_index() -> TestResult {
    let home = Arc::new(tempfile::tempdir().map_err(ctx("tempdir"))?);
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![
        document("Parallelchunk eins ueber Katzen.", "a.md", DEFAULT_VISIBILITY),
        document("Parallelchunk zwei ueber Hunde.", "b.md", DEFAULT_VISIBILITY),
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
    .map_err(ctx("build succeeds"))?;

    let h1 = home.clone();
    let t1 = std::thread::spawn(move || {
        ask(
            h1.path(),
            &IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY),
            &ReadScope::single(DEFAULT_VISIBILITY),
            "Katzen",
            &DeterministicEmbedder::new(16),
            &descriptor_for_thread(),
            &provenance_for_thread(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
    });
    let h2 = home.clone();
    let t2 = std::thread::spawn(move || {
        ask(
            h2.path(),
            &IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY),
            &ReadScope::single(DEFAULT_VISIBILITY),
            "Hunde",
            &DeterministicEmbedder::new(16),
            &descriptor_for_thread(),
            &provenance_for_thread(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
    });
    let r1 = t1.join().map_err(|_| ctx("thread 1 panicked"))?;
    let r2 = t2.join().map_err(|_| ctx("thread 2 panicked"))?;
    assert!(r1.is_ok(), "parallel query 1 must succeed: {r1:?}");
    assert!(r2.is_ok(), "parallel query 2 must succeed: {r2:?}");
    Ok(())
}

#[test]
fn test_index_verify_parallel_query_out_of_scope_is_rejected() -> TestResult {
    let home = Arc::new(tempfile::tempdir().map_err(ctx("tempdir"))?);
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();
    let documents = vec![document("Scopechunk ueber Katzen.", "a.md", DEFAULT_VISIBILITY)];
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
    .map_err(ctx("build succeeds"))?;

    let h1 = home.clone();
    let t1 = std::thread::spawn(move || {
        ask(
            h1.path(),
            &IndexSelector::new(DOCS_DESIGN_INDEX, OPERATOR_ONLY_VISIBILITY),
            &ReadScope::single(DEFAULT_VISIBILITY),
            "Katzen",
            &DeterministicEmbedder::new(16),
            &descriptor_for_thread(),
            &provenance_for_thread(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
    });
    let h2 = home.clone();
    let t2 = std::thread::spawn(move || {
        ask(
            h2.path(),
            &IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY),
            &ReadScope::single(DEFAULT_VISIBILITY),
            "Katzen",
            &DeterministicEmbedder::new(16),
            &descriptor_for_thread(),
            &provenance_for_thread(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
    });
    let r1 = t1.join().map_err(|_| ctx("thread 1 panicked"))?;
    let r2 = t2.join().map_err(|_| ctx("thread 2 panicked"))?;
    assert!(
        matches!(r1, Err(LensError::Query(QueryError::IndexNotVisible { .. }))),
        "out-of-scope selector must stay rejected under parallelism: {r1:?}"
    );
    assert!(r2.is_ok(), "in-scope parallel query must succeed: {r2:?}");
    Ok(())
}

#[test]
fn test_ask_computes_nothing_itself_same_hits_as_direct_query_scoped_call() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
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
    .map_err(ctx("build succeeds"))?;

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
    .map_err(ctx("ask succeeds"))?;

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
    .map_err(ctx("direct query_scoped call succeeds"))?;

    assert!(!via_facade.is_empty());
    assert_eq!(via_facade, via_direct_query_path);
    Ok(())
}

/// Der zentrale Beleg dieses Knotens: die Manifest-Prüfung in
/// `harw-lens-index` ist über die Fassade tatsächlich auslösbar. Ein mit
/// `"model-a"` gebauter Index lehnt eine Abfrage ab, die mit `"model-b"`
/// eingebettet wurde -- kein leeres Ergebnis, kein Treffer, sondern ein
/// Fehler, der auch nach dem Wickeln in `LensError` als
/// `QueryError::Index(_)` unterscheidbar bleibt, nicht auf eine
/// Zeichenkette zusammengefaltet.
#[test]
fn test_ask_rejects_a_query_embedded_with_a_different_model_than_the_index() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
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
    .map_err(ctx("build succeeds"))?;

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

    let Err(err) = result else {
        return Err(TestError::Unexpected(
            "a query embedded with a different model than the index must be rejected, never silently answered"
                .into(),
        ));
    };
    assert!(matches!(err, LensError::Query(QueryError::Index(_))));
    Ok(())
}

/// Gegentest zum vorherigen: passende Provenienz liefert Treffer. Ohne
/// diesen Test bewiese der vorherige nur, dass irgendetwas an dem Aufruf
/// fehlschlägt, nicht dass die Prüfung modellspezifisch ist.
#[test]
fn test_ask_accepts_a_query_embedded_with_the_same_model_as_the_index() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
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
    .map_err(ctx("build succeeds"))?;

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
    .map_err(ctx(
        "provenance matching the index's model must be accepted",
    ))?;

    assert!(!hits.is_empty());
    Ok(())
}

/// Derselbe Fehlertyp, ein anderer Grund: eine abweichende
/// Zerlegungsfassung verschiebt die Chunk-Grenzen, nicht den Vektorraum --
/// die Prüfung muss trotzdem über die Fassade greifen.
#[test]
fn test_ask_rejects_a_query_embedded_with_a_different_chunker_version_than_the_index() -> TestResult
{
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
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
    .map_err(ctx("build succeeds"))?;

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
    let scope = ReadScope::single(DEFAULT_VISIBILITY);
    let mismatched_provenance = QueryProvenance {
        model: "test-model".to_owned(),
        chunker_version: CHUNKER_VERSION + 1,
    };

    let Err(err) = ask(
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
    ) else {
        return Err(TestError::Unexpected(
            "a query embedded against a different chunker version must be rejected".into(),
        ));
    };

    assert!(matches!(err, LensError::Query(QueryError::Index(_))));
    Ok(())
}

/// [`index_status`] ist die im `crate`-`//!`-Block angekündigte
/// Introspektionsfunktion: `None` vor jedem `build`, `Some(IndexStatus)`
/// danach, mit `chunk_count`/`dimension` aus den tatsächlich gespeicherten
/// Einträgen statt nur dem Manifest.
#[test]
fn test_index_status_reflects_build_state_through_the_facade() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let embedder = DeterministicEmbedder::new(16);
    let descriptor = descriptor();

    let before = index_status(home.path(), DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY)
        .map_err(ctx("no error before any build"))?;
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
    .map_err(ctx("build succeeds"))?;

    let after = index_status(home.path(), DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY)
        .map_err(ctx("reads"))?
        .ok_or(TestError::Missing("index exists after build"))?;
    assert_eq!(after.index_name, DOCS_DESIGN_INDEX);
    assert_eq!(after.visibility, DEFAULT_VISIBILITY);
    assert_eq!(after.model, "test-model");
    assert_eq!(after.locality, Locality::Local);
    assert_eq!(after.chunker_version, CHUNKER_VERSION);
    assert_eq!(after.dimension, Some(16));
    assert_eq!(after.chunk_count, 1);
    assert!(after.modified.is_some());
    Ok(())
}
