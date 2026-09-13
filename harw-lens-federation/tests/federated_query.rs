//! Integrationstests für AW6-07: die Lens-Föderation über mehrere Indizes.
//!
//! Lebt bewusst als externer Integrationstest (`tests/`), nicht als
//! `#[cfg(test)]`-Modul in `src/`: er braucht `harw-lens-source` (nur als
//! Dev-Abhängigkeit dieser Crate, siehe `Cargo.toml`) für den Build-Schritt,
//! den `harw-lens-federation` zur Laufzeit nicht selbst besitzt -- Muster:
//! `harw-lens-query/tests/round_trip.rs`. Alle Tests nutzen `tempfile` und
//! `DeterministicEmbedder`; keiner spricht mit einem echten Modell oder einem
//! echten Netz.

use harw_knowledge::{
    AgentId, ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, KnowledgeIndex,
    VisibilityScope,
};
use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
use harw_lens_federation::{federated_query, FederationError, SkipReason};
use harw_lens_query::{IndexSelector, QueryError, QueryProvenance, ReadScope};
use harw_lens_source::{
    build_index, collect_design_docs, collect_palace_documents, CHUNKER_VERSION,
    DOCS_DESIGN_INDEX, KNOWLEDGE_PALACE_INDEX,
};
use harw_lens_types::{CollapsePolicy, EdgeIndex, Locality, Metric, SourceRef};

fn descriptor() -> EmbeddingDescriptor {
    EmbeddingDescriptor {
        document_prefix: "passage: ".to_owned(),
        query_prefix: "query: ".to_owned(),
        normalize: false,
    }
}

fn frontmatter(scope: VisibilityScope) -> Frontmatter {
    Frontmatter::new(AgentId::new("agent"), scope, jiff::Timestamp::now())
}

fn provenance(model: &str) -> QueryProvenance {
    QueryProvenance {
        model: model.to_owned(),
        chunker_version: CHUNKER_VERSION,
    }
}

/// Baut `docs.design` (aus einer Markdown-Datei) und `knowledge.palace` (aus
/// einem Wissens-Artefakt), beide unter `"workspace"`-Sichtbarkeit, mit
/// demselben `model`. Beide Texte teilen den Suchbegriff `"pipeline"`, damit
/// eine föderierte Abfrage nach diesem Begriff Treffer aus **beiden** Indizes
/// liefert.
fn build_two_workspace_indices(home: &std::path::Path, embedder: &DeterministicEmbedder, model: &str) {
    let docs_root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        docs_root.path().join("architecture.md"),
        "# Architecture\n\nThe deployment pipeline has three stages.\n",
    )
    .expect("write design doc");
    let documents = collect_design_docs(docs_root.path()).expect("collects design docs");
    build_index(
        home,
        DOCS_DESIGN_INDEX,
        &documents,
        model,
        Locality::Local,
        Metric::Cosine,
        embedder,
        &descriptor(),
    )
    .expect("builds docs.design");

    let mut index = KnowledgeIndex::new();
    index.insert(KnowledgeArtifact::new(
        ArtifactId::new("palace/deploy-pipeline"),
        ArtifactKind::PalaceNode,
        frontmatter(VisibilityScope::SelfOnly),
        "the release pipeline runs three stages in sequence",
    ));
    let palace_documents = collect_palace_documents(&index);
    build_index(
        home,
        KNOWLEDGE_PALACE_INDEX,
        &palace_documents,
        model,
        Locality::Local,
        Metric::Cosine,
        embedder,
        &descriptor(),
    )
    .expect("builds knowledge.palace");
}

/// Grundzusage: eine Föderation über zwei kompatible, sichtbare Indizes
/// liefert Treffer aus beiden, über RRF verschmolzen.
#[test]
fn federated_query_fuses_hits_from_both_indices() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    build_two_workspace_indices(home.path(), &embedder, "test-model");

    let selectors = [
        IndexSelector::new(DOCS_DESIGN_INDEX, "workspace"),
        IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "workspace"),
    ];
    let scope = ReadScope::single("workspace");

    let outcome = federated_query(
        home.path(),
        &selectors,
        &scope,
        "pipeline",
        &embedder,
        &descriptor(),
        &provenance("test-model"),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
        60.0,
    )
    .expect("federated query succeeds");

    assert_eq!(outcome.queried.len(), 2);
    assert!(outcome.skipped.is_empty());
    assert!(outcome.fused.iter().any(|hit| hit.chunk.source
        == SourceRef::File {
            path: "architecture.md".to_owned()
        }));
    assert!(outcome.fused.iter().any(|hit| hit.chunk.source
        == SourceRef::Artifact {
            id: "palace/deploy-pipeline".to_owned()
        }));
}

/// Determinismus: dieselbe Frage, dieselben Indizes, dieselbe Provenienz ->
/// dieselbe Reihenfolge, zweimal.
#[test]
fn federated_query_is_deterministic_across_repeated_calls() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    build_two_workspace_indices(home.path(), &embedder, "test-model");

    let selectors = [
        IndexSelector::new(DOCS_DESIGN_INDEX, "workspace"),
        IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "workspace"),
    ];
    let scope = ReadScope::single("workspace");

    let run = || {
        federated_query(
            home.path(),
            &selectors,
            &scope,
            "pipeline",
            &embedder,
            &descriptor(),
            &provenance("test-model"),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
            60.0,
        )
        .expect("federated query succeeds")
    };

    let first = run();
    let second = run();

    let first_order: Vec<_> = first.fused.iter().map(|r| r.chunk.digest).collect();
    let second_order: Vec<_> = second.fused.iter().map(|r| r.chunk.digest).collect();
    assert_eq!(first_order, second_order);
    assert_eq!(first.queried, second.queried);
}

/// Der wichtigste Sicherheitstest dieses Knotens: eine Föderation, die einen
/// Selektor außerhalb des Lesebereichs enthält, bricht **vollständig** ab --
/// nie ein stillschweigend gekürztes Ergebnis über die übrigen, sichtbaren
/// Indizes.
#[test]
fn federated_query_aborts_entirely_when_one_selector_is_outside_read_scope() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    build_two_workspace_indices(home.path(), &embedder, "test-model");

    let selectors = [
        IndexSelector::new(DOCS_DESIGN_INDEX, "workspace"),
        // Existiert nicht einmal auf Platte -- die Ablehnung muss trotzdem
        // vor jedem Dateisystemzugriff feststehen (siehe `resolve_index`).
        IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "operator-only"),
    ];
    let scope = ReadScope::single("workspace");

    let err = federated_query(
        home.path(),
        &selectors,
        &scope,
        "pipeline",
        &embedder,
        &descriptor(),
        &provenance("test-model"),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
        60.0,
    )
    .expect_err("a selector outside the read scope must abort the whole federation");

    assert!(matches!(
        err,
        FederationError::Query(QueryError::IndexNotVisible { .. })
    ));
}

/// Die physische Sichtbarkeitstrennung hält auch über die Föderation hinweg:
/// ein `OperatorOnly`-Artefakt in `knowledge.palace` ist über einen
/// `workspace`-Lesebereich weder einzeln noch föderiert erreichbar, wohl aber
/// über einen Lesebereich, der `operator-only` tatsächlich freigibt.
#[test]
fn federated_query_keeps_operator_only_index_invisible_to_workspace_scope() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);

    let mut index = KnowledgeIndex::new();
    index.insert(KnowledgeArtifact::new(
        ArtifactId::new("palace/secret"),
        ArtifactKind::PalaceNode,
        frontmatter(VisibilityScope::OperatorOnly),
        "operator only classified content nobody else should read",
    ));
    let documents = collect_palace_documents(&index);
    build_index(
        home.path(),
        KNOWLEDGE_PALACE_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor(),
    )
    .expect("builds knowledge.palace");

    let selectors = [IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "operator-only")];

    let workspace_scope = ReadScope::single("workspace");
    let err = federated_query(
        home.path(),
        &selectors,
        &workspace_scope,
        "classified",
        &embedder,
        &descriptor(),
        &provenance("test-model"),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
        60.0,
    )
    .expect_err("operator-only visibility is outside the workspace scope");
    assert!(matches!(
        err,
        FederationError::Query(QueryError::IndexNotVisible { .. })
    ));

    let operator_scope = ReadScope::single("operator-only");
    let outcome = federated_query(
        home.path(),
        &selectors,
        &operator_scope,
        "classified content",
        &embedder,
        &descriptor(),
        &provenance("test-model"),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
        60.0,
    )
    .expect("a caller actually scoped to operator-only can reach it");
    assert!(!outcome.fused.is_empty());
}

/// Ein Index mit abweichendem Modell wird nicht mitfusioniert -- über den
/// öffentlichen Weg (`federated_query`) auslösbar, nicht nur im
/// Einzeltest von `harw-lens-rank`/`harw-lens-index`. Die Föderation bricht
/// dabei nicht ab: die übrigen, kompatiblen Indizes liefern weiterhin ein
/// Ergebnis, und `skipped` macht den Ausschluss sichtbar.
#[test]
fn federated_query_skips_index_with_incompatible_model_without_aborting() {
    let home = tempfile::tempdir().expect("tempdir");
    let embedder = DeterministicEmbedder::new(16);
    build_two_workspace_indices(home.path(), &embedder, "test-model");

    // knowledge.palace wird zusaetzlich mit einem abweichenden Modell
    // ueberschrieben -- derselbe Indexname, aber ein Manifest, das nicht mehr
    // zur Provenienz der Abfrage passt.
    let mut index = KnowledgeIndex::new();
    index.insert(KnowledgeArtifact::new(
        ArtifactId::new("palace/deploy-pipeline"),
        ArtifactKind::PalaceNode,
        frontmatter(VisibilityScope::SelfOnly),
        "the release pipeline runs three stages in sequence",
    ));
    let palace_documents = collect_palace_documents(&index);
    build_index(
        home.path(),
        KNOWLEDGE_PALACE_INDEX,
        &palace_documents,
        "model-b",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor(),
    )
    .expect("rebuilds knowledge.palace with a different model");

    let selectors = [
        IndexSelector::new(DOCS_DESIGN_INDEX, "workspace"),
        IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "workspace"),
    ];
    let scope = ReadScope::single("workspace");

    let outcome = federated_query(
        home.path(),
        &selectors,
        &scope,
        "pipeline",
        &embedder,
        &descriptor(),
        &provenance("test-model"),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
        60.0,
    )
    .expect("the federation itself must not abort over one incompatible index");

    assert_eq!(outcome.queried, vec![IndexSelector::new(DOCS_DESIGN_INDEX, "workspace")]);
    assert_eq!(outcome.skipped.len(), 1);
    assert_eq!(
        outcome.skipped[0].selector,
        IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "workspace")
    );
    assert_eq!(
        outcome.skipped[0].reason,
        SkipReason::IncompatibleManifest { field: "model" }
    );
    // Der abweichende Index darf keinen einzigen Treffer beisteuern.
    assert!(outcome.fused.iter().all(|hit| hit.chunk.source
        != SourceRef::Artifact {
            id: "palace/deploy-pipeline".to_owned()
        }));
}
