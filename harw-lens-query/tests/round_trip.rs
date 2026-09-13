//! Integrationstests für AW5-08: der volle Weg von rohen Quellen
//! (`harw-lens-source`) über den gebauten Index bis zur Abfrage
//! (`harw-lens-query`).
//!
//! Diese Tests leben bewusst als externe Integrationstests dieser Crate
//! (`tests/`), nicht als `#[cfg(test)]`-Modul in `src/`: sie brauchen
//! `harw-lens-source` (nur als Dev-Abhängigkeit dieser Crate, siehe
//! `Cargo.toml`) für den Build-Schritt, den `harw-lens-query` zur Laufzeit
//! nicht selbst besitzt. Alle Tests nutzen `tempfile` und
//! `DeterministicEmbedder` — keiner spricht mit einem echten Modell oder
//! einem echten Netz.

use harw_knowledge::{
    AgentId, ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, KnowledgeIndex,
    VisibilityScope,
};
use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
use harw_lens_query::{query_scoped, IndexSelector, QueryError, QueryProvenance, ReadScope};
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

/// Die zu jedem `build_index`-Aufruf in dieser Datei passende Provenienz --
/// dasselbe Modell (`"test-model"`) und dieselbe Zerlegungsfassung, mit der
/// `build_index` seine Indizes tatsächlich gebaut hat.
fn provenance() -> QueryProvenance {
    QueryProvenance {
        model: "test-model".to_owned(),
        chunker_version: CHUNKER_VERSION,
    }
}

/// `docs.design` wird gebaut und ist abfragbar; ein Treffer zeigt auf den
/// richtigen `SourceRef`.
#[test]
fn docs_design_round_trip_finds_the_right_source_ref() {
    let home = tempfile::tempdir().expect("tempdir");
    let docs_root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        docs_root.path().join("architecture.md"),
        "# Architecture\n\nThe system has three layers.\n",
    )
    .expect("write design doc");

    let documents = collect_design_docs(docs_root.path()).expect("collects");
    let embedder = DeterministicEmbedder::new(16);
    build_index(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor(),
    )
    .expect("builds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, "workspace");
    let scope = ReadScope::single("workspace");
    let hits = query_scoped(
        home.path(),
        &selector,
        &scope,
        "three layers",
        &embedder,
        &descriptor(),
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("query succeeds");

    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].chunk.source,
        SourceRef::File {
            path: "architecture.md".to_owned()
        }
    );
}

/// `knowledge.palace` wird gebaut und ist abfragbar; ein Treffer zeigt auf
/// den richtigen `SourceRef`.
#[test]
fn knowledge_palace_round_trip_finds_the_right_source_ref() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut index = KnowledgeIndex::new();
    index.insert(KnowledgeArtifact::new(
        ArtifactId::new("palace/deploy-pipeline"),
        ArtifactKind::PalaceNode,
        frontmatter(VisibilityScope::SelfOnly),
        "the deploy pipeline runs three stages in sequence",
    ));

    let documents = collect_palace_documents(&index);
    let embedder = DeterministicEmbedder::new(16);
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
    .expect("builds");

    let selector = IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "workspace");
    let scope = ReadScope::single("workspace");
    let hits = query_scoped(
        home.path(),
        &selector,
        &scope,
        "deploy pipeline",
        &embedder,
        &descriptor(),
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("query succeeds");

    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].chunk.source,
        SourceRef::Artifact {
            id: "palace/deploy-pipeline".to_owned()
        }
    );
}

/// Der wichtigste Sicherheitstest dieses Knotens: ein `OperatorOnly`-Artefakt
/// landet beim Build im getrennten physischen Index und ist über den
/// gewöhnlichen (`workspace`) weder per Selektor noch per Treffer
/// auffindbar — wohl aber über einen Lesebereich, der `operator-only`
/// tatsächlich freigibt.
#[test]
fn operator_only_artifact_is_unreachable_through_the_workspace_scope() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut index = KnowledgeIndex::new();
    index.insert(KnowledgeArtifact::new(
        ArtifactId::new("palace/normal"),
        ArtifactKind::PalaceNode,
        frontmatter(VisibilityScope::SelfOnly),
        "ordinary content anyone in the workspace may read",
    ));
    index.insert(KnowledgeArtifact::new(
        ArtifactId::new("palace/secret"),
        ArtifactKind::PalaceNode,
        frontmatter(VisibilityScope::OperatorOnly),
        "operator only classified content nobody else should read",
    ));

    let documents = collect_palace_documents(&index);
    let embedder = DeterministicEmbedder::new(16);
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
    .expect("builds");

    // A caller scoped to "workspace" only cannot even select the
    // operator-only index: the request is rejected outright, before any
    // disk access -- never a silent empty result.
    let workspace_scope = ReadScope::single("workspace");
    let operator_selector = IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "operator-only");
    let err = query_scoped(
        home.path(),
        &operator_selector,
        &workspace_scope,
        "classified",
        &embedder,
        &descriptor(),
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect_err("operator-only visibility is outside the workspace scope");
    assert!(matches!(err, QueryError::IndexNotVisible { .. }));

    // Even querying the ordinary workspace index directly never turns up the
    // secret artifact's chunk: it was never written there in the first
    // place (a physically separate index, not a filter over a shared one).
    let workspace_selector = IndexSelector::new(KNOWLEDGE_PALACE_INDEX, "workspace");
    let workspace_hits = query_scoped(
        home.path(),
        &workspace_selector,
        &workspace_scope,
        "classified content",
        &embedder,
        &descriptor(),
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("query succeeds");
    assert!(workspace_hits.iter().all(|hit| hit.chunk.source
        != SourceRef::Artifact {
            id: "palace/secret".to_owned()
        }));

    // But a caller actually scoped to "operator-only" can reach it.
    let operator_scope = ReadScope::single("operator-only");
    let operator_hits = query_scoped(
        home.path(),
        &operator_selector,
        &operator_scope,
        "classified content",
        &embedder,
        &descriptor(),
        &provenance(),
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect("query succeeds");
    assert!(!operator_hits.is_empty());
    assert_eq!(
        operator_hits[0].chunk.source,
        SourceRef::Artifact {
            id: "palace/secret".to_owned()
        }
    );
}

/// Der Beleg, dass die Manifest-Prüfung über den vorgesehenen öffentlichen
/// Weg tatsächlich auslösbar ist: ein mit `"model-a"` gebauter Index lehnt
/// eine Abfrage ab, die mit `"model-b"` eingebettet wurde -- kein leeres
/// Ergebnis, kein Treffer, sondern ein Fehler. Vor dieser Änderung baute
/// `query` sein Abfrage-Manifest aus `index.manifest().clone()`, also aus
/// genau dem Index, den es durchsuchte -- dieser Zustand war über
/// `query_scoped`/`query` nicht erreichbar.
#[test]
fn query_scoped_rejects_a_query_embedded_with_a_different_model_than_the_index() {
    let home = tempfile::tempdir().expect("tempdir");
    let docs_root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        docs_root.path().join("architecture.md"),
        "# Architecture\n\nThe system has three layers.\n",
    )
    .expect("write design doc");

    let documents = collect_design_docs(docs_root.path()).expect("collects");
    let embedder = DeterministicEmbedder::new(16);
    build_index(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "model-a",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor(),
    )
    .expect("builds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, "workspace");
    let scope = ReadScope::single("workspace");
    let mismatched_provenance = QueryProvenance {
        model: "model-b".to_owned(),
        chunker_version: CHUNKER_VERSION,
    };

    let err = query_scoped(
        home.path(),
        &selector,
        &scope,
        "three layers",
        &embedder,
        &descriptor(),
        &mismatched_provenance,
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect_err("a query embedded with a different model than the index must be rejected");

    assert!(matches!(err, QueryError::Index(_)));
}

/// Gegentest zum vorherigen: passende Provenienz (dasselbe Modell, mit dem
/// der Index gebaut wurde) liefert Treffer. Ohne diesen Test bewiese der
/// vorherige nur, dass irgendetwas an dem Aufruf fehlschlägt, nicht dass die
/// Prüfung modellspezifisch ist.
#[test]
fn query_scoped_accepts_a_query_embedded_with_the_same_model_as_the_index() {
    let home = tempfile::tempdir().expect("tempdir");
    let docs_root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        docs_root.path().join("architecture.md"),
        "# Architecture\n\nThe system has three layers.\n",
    )
    .expect("write design doc");

    let documents = collect_design_docs(docs_root.path()).expect("collects");
    let embedder = DeterministicEmbedder::new(16);
    build_index(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "model-a",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor(),
    )
    .expect("builds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, "workspace");
    let scope = ReadScope::single("workspace");
    let matching_provenance = QueryProvenance {
        model: "model-a".to_owned(),
        chunker_version: CHUNKER_VERSION,
    };

    let hits = query_scoped(
        home.path(),
        &selector,
        &scope,
        "three layers",
        &embedder,
        &descriptor(),
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
/// die Prüfung muss trotzdem greifen.
#[test]
fn query_scoped_rejects_a_query_embedded_with_a_different_chunker_version_than_the_index() {
    let home = tempfile::tempdir().expect("tempdir");
    let docs_root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        docs_root.path().join("architecture.md"),
        "# Architecture\n\nThe system has three layers.\n",
    )
    .expect("write design doc");

    let documents = collect_design_docs(docs_root.path()).expect("collects");
    let embedder = DeterministicEmbedder::new(16);
    build_index(
        home.path(),
        DOCS_DESIGN_INDEX,
        &documents,
        "test-model",
        Locality::Local,
        Metric::Cosine,
        &embedder,
        &descriptor(),
    )
    .expect("builds");

    let selector = IndexSelector::new(DOCS_DESIGN_INDEX, "workspace");
    let scope = ReadScope::single("workspace");
    let mismatched_provenance = QueryProvenance {
        model: "test-model".to_owned(),
        chunker_version: CHUNKER_VERSION + 1,
    };

    let err = query_scoped(
        home.path(),
        &selector,
        &scope,
        "three layers",
        &embedder,
        &descriptor(),
        &mismatched_provenance,
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        10,
    )
    .expect_err("a query embedded against a different chunker version must be rejected");

    assert!(matches!(err, QueryError::Index(_)));
}
