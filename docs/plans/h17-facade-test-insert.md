# Übergabe: h17-Testeinfügung in harw-lens/tests/facade.rs

Einfügeposition (vom Writer verifiziert): vor dem `#[test]` von
`test_ask_computes_nothing_itself_same_hits_as_direct_query_scoped_call` (Z.218).

Vor den Tests zwei Thread-Helfer einfügen — Kopie der bestehenden
`descriptor()`- bzw. `provenance()`-Bodies (identischer Inhalt, nur als freie
Funktionen; wenn Descriptor/Provenance bereits Clone + Send sind, stattdessen
vor dem Spawn klonen und per `move` capturen — dann Helfer weglassen):

```rust
fn descriptor_for_thread() -> <Typ wie descriptor()> {
    // identischer Body wie descriptor()
}

fn provenance_for_thread() -> <Typ wie provenance()> {
    // identischer Body wie provenance()
}
```

Danach die sechs Tests:

```rust
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
        ..document("Nicht-UTF-8-Pfad-Chunk ueber Katzen.", "gut.md", DEFAULT_VISIBILITY)
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
```

Hinweise zur Anpassung (Quelltreue vor wörtlicher Übernahme):
- `ctx("…")` im join-Fehlerpfad statt `TestError::Unexpected` verwenden, falls
  kein TestError-Typ existiert — an das Muster der Datei anpassen.
- Fehler-Varianten (`LensError::Query(QueryError::IndexNotVisible)`) gegen die
  echten Pfade prüfen; wenn `ask` bereits `LensError` wickelt, wie in den
  Bestandstests matches! verwenden.
- Diese Datei ist nur Übergabe-Vorlage; nach der Einfügung kann sie entfernt
  oder als research/h17-Beleg verschoben werden.
