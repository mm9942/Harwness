# `harw-lens`: Knowledge Index, Vector Store and Retrieval for Harwness

> Status: implemented · Last reviewed: 2026-09-24

**Purpose:** a retrieval subsystem for Harwness: chunking, embeddings,
vector and lexical index, hybrid search, relation resolution.
**Related:** `docs/design/runtime-contracts.md`,
`harw-dod-integration-and-dependencies.md` (dependency doctrine)

---

## 0. The name and the ownership boundary

**`harw-lens`.** A lens focuses; it doesn't own what it looks at. That's
the boundary: Lens indexes and ranks, but owns no knowledge. Artifacts
belong to `harw-knowledge`, signals to `harw-memory`, transcripts to
`harw-session-store`, source code to the workspace. Lens never deletes
anything; it only knows digests and pointers.

---

## 1. Origins: what was kept from prior art and what wasn't

This design drew on ideas from an earlier, separate retrieval project
(referred to here only by its ideas, not its name), used as inspiration,
not a template.

### 1.1 Kept

**The index as a content-addressed artifact with a manifest.** An index is
written, read back by digest, and carries a manifest with version, backend,
dimension and row count — fitting Harwness' existing blake3-digest and
snapshot-ID world exactly. **Implemented:** `IndexManifest` in
`harw-lens-types`.

**A `VectorIndex` trait with swappable backends.** Flat first, ANN later,
behind a trait — the same discipline as the provider layer. **Implemented:**
`harw-lens-index` (`FlatIndex`, plus a BM25 index).

**BM25 plus RRF** to fuse dense and lexical rankings. **Implemented:**
`harw-lens-rank` (`bm25`, `fuse`).

**Reranking with MMR** (maximal marginal relevance), turning "the ten most
similar" into "the ten usefully different." **Implemented:**
`harw-lens-rank::mmr`.

**Relation expansion and category-aware collapsing at dedup time.**
**Partially implemented** — see §5.3: the shipped `collapse` distinguishes
two edge kinds (`SupersededBy`, `Contradicts`), not the fuller five-category
scheme originally sketched here.

**Chunking as a contract that proposes, not asserts.** **Implemented:**
`harw-lens-chunk` produces chunk and relation *proposals*.

**External linking without owning the referenced domain.** Lens references
workspace files and dependency sources by pointer, never copies or
interprets them. **Implemented:** `SourceRef` in `harw-lens-types`.

**An `EmbeddingProvider` trait with a registry**, mirroring Harwness'
provider pattern. **Implemented:** `harw-lens-embed`.

### 1.2 Deliberately not carried over

- **A multi-tenant Postgres catalog.** Harwness is single-host and
  file-based; the tenancy axis is replaced by `VisibilityScope`, which
  already exists and is sharper for this purpose.
- **Python worker subprocesses for chunking.** Chunking is built in Rust,
  deterministic and testable — no subprocess execution permission needed
  for what is pure computation.
- **A standalone HTTP API or MCP namespace.** Lens gets operations
  (`harw-ops`) and an agent tool (`harw-tool-lens`), not its own service.
- **UUID keys everywhere.** Harwness uses typed newtypes and content
  digests; a chunk has a digest, not a random ID.
- **A daemon.** Lens is a library plus jobs on the existing job bridge, not
  a server process.

---

## 2. The gap Lens closes

`harw-knowledge::recall` is keyword-based (BM25-style, with hard caps on
artifact count and hop depth) — a good foundation and also a ceiling:

| Before Lens | With Lens |
|---|---|
| lexical only | dense plus lexical, fused via RRF |
| index built at query time | index is a persisted, digest-addressed artifact |
| hard constants for recall caps | budget as a query parameter, capped by `ContextCeiling` |
| artifact is the smallest unit | chunk is the smallest unit, artifact stays the identity |
| edges used only for traversal | edges also used for deduplication |
| knowledge artifacts only | also workspace files, dependency sources, transcripts |

The last point opens the most new ground: `harw-code-graph` already
locates registry source directories and builds docs.rs URLs. An index over
the source of one's own dependencies is the basis for an "analysis mode"
that can answer "where in the tree does this happen" without a model
guessing.

---

## 3. Normative invariants

**L1. Lens owns nothing.** It stores digests, pointers, vectors and
rankings. Deletion is never Lens' job; `superseded_by` is respected, not
enforced.

**L2. An index is an artifact with a manifest.** Model ID, dimension,
metric, chunker version, source-set digest and backend are all in the
manifest. An index with a different model is a different index, not an
updated one.

**L3. Visibility is an index boundary, not a filter.** Physically separate
indices per visibility class. Rationale in §4.2.

**L4. `OperatorOnly` content is indexed only with local embeddings.** A
remote embedding call is a transmission of the content. The provider
carries a `Locality`, the manifest records it, and the build rejects the
combination fail-closed.

**L5. Chunking is deterministic and reproducible.** Same text, same
chunker version, same chunks with the same digests.

**L6. Relation proposals are proposed, not written.** A chunker that
detects a relationship produces a proposal that enters the knowledge graph
as provisional and is promoted review-gated.

**L7. Retrieval is pure.** Ranking, fusion, reranking and collapsing are
functions with no I/O and no system clock.

**L8. No chunk content in telemetry.** Only numbers, digests and reasons
are measured.

---

## 4. Crate landscape

### 4.1 Layout

Implemented as eleven crates (`dod`-style prefix discipline, but under the
product workspace, not `dod/`):

| Crate | Responsibility |
|---|---|
| `harw-lens-types` | `ChunkId`, `ChunkDigest`, `Embedding`, `Metric`, `Hit`/`Ranked`, `IndexId`, `IndexManifest`, `SourceRef`, `Locality`, `EdgeIndex`, `CollapsePolicy` |
| `harw-lens-chunk` | deterministic chunking (`chunk_markdown`, `chunk_rust`, `chunk_plain`), relation proposals |
| `harw-lens-rank` | pure ranking: `fuse` (RRF), `mmr`, `collapse`, `pack`, `bm25` |
| `harw-lens-embed` | `EmbeddingProvider` trait and registry, locality tracking |
| `harw-lens-index` | `VectorIndex` trait, `FlatIndex`, a BM25 index, persistence and manifest |
| `harw-lens-store` | content-addressed chunk/index storage, advisory locking, atomic rename |
| `harw-lens-source` | source adapters (read-only, scope-bound) and incremental index build/status |
| `harw-lens-query` | single-index query path: search modes, expansion, visibility enforcement |
| `harw-lens-federation` | queries across multiple indices, built on `harw-lens-query`'s single-index primitives rather than a new selector/visibility type |
| `harw-lens` | facade: re-export, prelude, no logic |
| `harw-tool-lens` | agent tool (`lens.search`, `lens.expand`) |

No daemon, no server, no database.

### 4.2 Why visibility is an index boundary (L3)

The obvious design would be one index over everything, filtered by
`VisibilityScope` at query time. That's convenient and wrong.

First, filtering after retrieval is a bug class: a forgotten filter is a
disclosure — exactly the kind of forgotten check Harwness builds authority
as a data structure to avoid.

Second, ranking itself leaks even when the filter works: how many total
hits exist, how strong the filtered-out scores were, whether a search
finds anything at all — these are side channels that reconstruct content
across repeated queries.

Hence: **one physical index per visibility class.** `OperatorOnly` content
lives in its own index that a normal agent never opens, because its scope
doesn't include it. The boundary is a path, not an `if` — the same idea as
the sensors' `ReadScope`.

The price is redundancy for content visible in multiple classes, which is
acceptable: chunks are content-addressed and stored once; only the vectors
are duplicated.

---

## 5. Core design

### 5.1 Vocabulary

Representative shape (see `harw-lens-types` for the exact current types):

```rust
pub struct ChunkDigest([u8; 32]);          // blake3

pub struct Chunk {
    pub digest: ChunkDigest,
    pub source: SourceRef,
    pub span: ByteSpan,
    pub text: String,
    pub kind: ChunkKind,                    // Prose | Code | Heading | Table | Frontmatter
}

/// A pointer to content Lens does not own or interpret (L1).
pub enum SourceRef {
    Artifact(ArtifactId),
    WorkspaceFile { path: PathBuf, rev: RepoRevision },
    DependencySource { package: String, version: String, path: PathBuf },
    Transcript { session: SessionId, turn: TurnId },
}

pub struct IndexManifest {
    pub id: IndexId,
    pub backend: &'static str,
    pub metric: Metric,
    pub dim: usize,
    pub model: EmbeddingModelId,
    pub locality: Locality,                 // Local | Remote  (L4)
    pub chunker_version: u32,                // (L5)
    pub visibility: VisibilityScope,         // (L3)
    pub source_set_digest: [u8; 32],
    pub n_chunks: usize,
    pub built_at: jiff::Timestamp,
}
```

### 5.2 Chunking

Pure, deterministic, format-aware. Three implemented strategies:

**Markdown** along heading hierarchy, overlap only at paragraph breaks,
frontmatter handled separately.

**Rust** along syntactic boundaries: item, `impl` block, module. Doc
comments stay with their item; a module doc (`//!`) is its own,
higher-weighted chunk, since module docs carry the real specification in
this workspace.

**Plain text** with paragraph/sentence boundaries as a fallback.

Relation proposals fall out of what the chunker already sees: heading
hierarchy yields `ChildOf`, links and Rust paths yield `References`,
consecutive chunks yield `FollowedBy`. They enter the knowledge graph as
provisional (L6).

### 5.3 Category-aware collapsing

**Implemented, in simplified form.** `harw-lens-rank::collapse` currently
distinguishes two edge kinds via `EdgeIndex`:

| Edge | Behavior on collapse |
|---|---|
| `SupersededBy` | collapse — the newer version survives |
| `Contradicts` | **never collapse** — both versions are kept |

This is deliberately narrower than the five-category scheme originally
proposed here (cluster binding, provenance, tension, reference, sequence).
The `Contradicts` case is the one that matters most: a contradiction is
exactly what an agent needs to see, and a pure similarity search would
otherwise treat the two conflicting statements as duplicates and discard
one. Extending `EdgeIndex` with cluster/reference/sequence categories
remains open (§10).

### 5.4 Query

Representative shape:

```rust
pub struct LensQuery {
    pub text: String,
    pub mode: SearchMode,                  // Dense | Sparse | Hybrid { k_rrf: u32 }
    pub budget: RetrievalBudget,           // top_k, max_chunks, max_tokens, max_hops
    pub sources: Vec<SourceFilter>,
    pub expand: Option<ExpandSpec>,
    pub collapse: CollapsePolicy,
    pub rerank: RerankPolicy,              // Identity | Mmr { lambda }
}

/// Pure (L7): no I/O, no system clock.
pub fn rank(dense: &[Hit], sparse: &[Hit], q: &LensQuery) -> Vec<Hit>;
```

`RetrievalBudget` replaces the old hard constants and is capped by the
calling agent's `ContextCeiling`: an agent cannot pull more context than
its program allows.

### 5.5 Building and incremental update

Because chunks are content-addressed, a rebuild is a diff: only chunks with
an unrecognized digest get embedded. Index builds run as jobs on the
existing job bridge, not a separate scheduler — the same pattern as the
security subsystem.

---

## 6. Embeddings and dependencies

### 6.1 The trait

```rust
pub trait EmbeddingProvider: Send + Sync {
    fn model(&self) -> EmbeddingModelId;
    fn dim(&self) -> usize;
    fn locality(&self) -> Locality;                 // (L4)
    fn embed<'a>(&'a self, texts: &'a [String]) -> ProviderFuture<'a, Vec<Embedding>>;
}
```

`Locality` is the checkpoint for L4: an index build rejects a `Remote`
provider for `OperatorOnly` content before any text leaves the machine.

### 6.2 Backends: status

**Implemented:** embeddings via the existing HTTP provider infrastructure
(OpenAI-compatible, Azure, local servers via a configured base URL) —
`Locality::Local` is achievable through a local server (e.g. an
Ollama-style local endpoint) without adding any ML dependency, and
`harw-lens-embed` has no `candle` or `ort`/ONNX dependency in its
`Cargo.toml` as of this review.

**Open:** in-process embedding inference (a pure-Rust encoder via a
Candle-style backend, or ONNX Runtime behind an optional feature for
non-security contexts) has not been built. The doctrine's preference —
keep any heavy ML dependency behind the trait, out of the core — still
holds; a remote/local-HTTP-only setup satisfies L4 already, so in-process
inference remains a later addition rather than a blocking gap.

### 6.3 Index backends

**Implemented:** `FlatIndex` (exact, brute-force vector search) in
`harw-lens-index`, sized for the current corpus (knowledge vault,
workspace, transcripts — tens to a few hundred thousand chunks).

**Open:** an ANN backend (e.g. HNSW) behind the same `VectorIndex` trait,
to be added only once a measurement calls for it.

---

## 7. Wiring into the existing system

| Existing element | Wiring |
|---|---|
| `harw-knowledge::recall` | stays as the fast lexical path; Lens is the second, richer route |
| `harw-knowledge` palace | source of edges and `superseded_by`; target for relation proposals as provisional |
| `harw-memory` contradiction tracking | source of `Contradicts` edges, so contradictions survive collapsing |
| `harw-code-graph` | source of workspace structure and dependency source directories |
| `harw-context` | `RetrievalBudget` capped by `ContextCeiling`; Lens hits become `TrustClass::Data` fragments |
| `harw-plan-bridge` | index builds run as plan nodes over the job bridge |
| `harw-tools` | `harw-tool-lens` with `lens.search`/`lens.expand` |
| `harw-ops` | index build/inspect operations |
| `harw-observe` | per-index metrics: chunk count, build time, hit rate, collapse rate |

**What agents may do:** search and expand. **What they may not do:** build,
delete, or change the visibility of an index — those are operations with a
permission tier, not tools.

---

## 8. Build stages (historical)

The original plan sequenced the work as: vocabulary → chunking → store and
index → embeddings → sources → query → tool wiring → extension (dependency
sources, transcripts as sources, an in-process embedding backend, ANN on
measured need). All stages through query and tool wiring are implemented,
per the crate landscape in §4; the extension stage's ANN backend and
in-process embeddings remain open (§6).

---

## 9. Checks

- **Chunking determinism.** Same input, same digests, via property tests
  and a golden corpus from the repository itself.
- **Manifest discipline.** A query against an index with a different model
  or chunker version is rejected, not silently answered.
- **L4 fail-closed.** A build over `OperatorOnly` content with a remote
  provider fails before any text leaves the machine.
- **L3 as a path boundary.** An agent without the matching scope cannot
  open the `OperatorOnly` index, even for reading.
- **Collapse fixtures.** A corpus with a real contradiction pair: both hits
  must survive. A pair with `superseded_by`: only the current version
  survives.
- **Purity.** `rank` runs with no I/O and no clock; property-tested over
  permutations of input order.
- **Incrementality.** Two builds in sequence with one changed file: the
  number of embedding calls matches the number of changed chunks.

---

## 10. Open decisions

1. **Full five-category collapse scheme.** Extend `EdgeIndex` beyond
   `SupersededBy`/`Contradicts` to cluster-binding, reference and sequence
   categories, or keep the two-category scheme if it proves sufficient.
2. **BM25 duplication.** `harw-knowledge` has its own BM25-style recall,
   and `harw-lens-rank`/`harw-lens-index` now have another. Whether
   `harw-knowledge` should migrate to consume the Lens implementation is
   still open.
3. **Chunk size and overlap tuning**, pending measurement against real
   corpora.
4. **Transcripts as a source.** The largest volume, the lowest
   signal-to-noise ratio. Proposal: index diary entries and summaries
   first, raw transcripts only once a concrete need is measured.
5. **In-process embedding backend.** Whether to add a Candle-based backend
   for offline operation, and when.
