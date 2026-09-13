//! Fassade des Retrieval-Subsystems — der einzige Name, den der Rest des
//! Workspaces von Lens kennt.
//!
//! # Warum dieser Knoten existiert
//! Lens besteht aus zehn Crates (`harw-lens-types`, `-rank`, `-chunk`,
//! `-store`, `-index`, `-embed`, `-source`, `-query`, `-federation` und
//! diese Crate selbst). Ohne Fassade müsste jeder Konsument wissen, dass
//! Chunking in `harw-lens-chunk`, Rangbildung in `harw-lens-rank`,
//! Speicherung in `harw-lens-store` und Abfrage in `harw-lens-query` liegt.
//! Mit dieser Fassade kennt er einen Namen (`harw-lens`) und zwei
//! Funktionen: [`build`] und [`ask`].
//!
//! `harw-lens-federation` ist zum Zeitpunkt dieses Knotens noch Gerüst
//! (Inhalt entsteht erst in Knoten AW6-07, nach diesem Knoten) und taucht
//! deshalb in der Fassadenfläche unten nicht auf: es gibt noch keinen
//! gelandeten Namen, den diese Crate auswählen oder verschweigen könnte.
//! Sobald AW6-07 landet, ist zu prüfen, ob ein föderiertes Abfragen über
//! mehrere Indizes eine dritte Fassadenfunktion (etwa `ask_federated`)
//! rechtfertigt — das ist bewusst nicht Teil dieses Knotens.
//!
//! # Das Auswahlkriterium
//! **Eine Fassade wählt aus, sie reicht nicht alles weiter.** Das Kriterium
//! für „gehört zur Fassadenfläche" ist nicht „ist `pub` in einer
//! Innencrate", sondern:
//!
//! 1. **Ist es Parameter- oder Rückgabetyp von [`build`]/[`ask`] selbst?**
//!    Dann muss der Aufrufer es zwingend benennen können — sonst kann er die
//!    beiden einzigen Funktionen, die diese Fassade anbietet, nicht
//!    aufrufen. (`Chunk`, `SourceRef`, `Locality`, `Metric`, `Ranked`,
//!    `RawDocument`, `IndexBuildReport`, `IndexSelector`, `ReadScope`,
//!    `EdgeIndex`, `CollapsePolicy`, `Embedder`, `EmbeddingDescriptor`, …)
//! 2. **Ist es ein Werkzeug, ohne das ein Aufrufer die Parameter aus (1)
//!    nicht sinnvoll befüllen kann**, ohne dabei selbst physische
//!    Innenteile zu berühren? (`collect_design_docs`,
//!    `collect_palace_documents`, `visibility_of_scope`, `EmbeddingCatalog`
//!    + `EmbeddingRole` + `route`, `DeterministicEmbedder` für Tests.)
//! 3. **Würde das Zurückhalten dieses Namens eine Entscheidung
//!    verschweigen, die eigentlich dem Aufrufer gehört?** Das ist der Grund,
//!    warum `EdgeIndex`, `EdgeKind` und `CollapsePolicy` zur Fläche gehören,
//!    obwohl sie „nur" Parameter sind, statt in `ask` versteckt zu werden
//!    (siehe `ask.rs`s `//!`-Block) — und der Grund, warum `IndexManifest`,
//!    `LensStore`, `FlatIndex`/`Bm25Index`/`VectorIndex`, `chunk_*` und
//!    `collapse`/`rrf_fuse`/`mmr`/`pack` **nicht** zur Fläche gehören: sie
//!    sind Werkzeuge, mit denen `harw-lens-source`/`harw-lens-query` ihre
//!    eigenen, bereits getroffenen Entscheidungen umsetzen, nicht
//!    Werkzeuge, die der Aufrufer selbst in der Hand halten müsste.
//!
//! Jeder Name unten wurde gegen diese drei Fragen geprüft; die Begründung
//! steht direkt daneben, nicht nur „Ja"/„Nein" — wer später ergänzen will,
//! muss das Kriterium anwenden können, nicht nur die Liste kopieren.
//!
//! # Die vollständige Fassadenfläche
//!
//! ## `harw-lens-types` — reines Vokabular
//! - **Ja** — [`Chunk`], [`ChunkDigest`], [`SourceRef`]: Feld von
//!   [`Ranked`] bzw. von `Chunk` selbst; jeder Treffer aus [`ask`] trägt
//!   sie. `ChunkDigest` zusätzlich, weil man ihn braucht, um über
//!   [`EdgeIndex::insert`] kuratierte Kanten zwischen zwei Chunks
//!   einzutragen.
//! - **Ja** — [`Locality`], [`Metric`]: Pflichtparameter von [`build`].
//! - **Ja** — [`Ranked`]: Rückgabetyp von [`ask`].
//! - **Ja** — [`CollapsePolicy`], [`EdgeKind`], [`EdgeIndex`]: explizite
//!   Parameter von [`ask`] (siehe Kriterium 3 oben und `ask.rs`). Das ist
//!   eine bewusste Korrektur gegenüber der ursprünglichen Grobsortierung
//!   dieses Knotens, die `EdgeIndex` noch bei den Innenteilen einordnete:
//!   `harw-lens-source` leitet zum jetzigen Zeitpunkt **keinen**
//!   `EdgeIndex` automatisch aus `harw-knowledge`-Daten ab (siehe
//!   `harw-lens-source`s `//!`-Block, Abschnitt „Was diese Crate nicht
//!   tut"). Würde [`ask`] diesen Parameter verstecken und intern still
//!   `EdgeIndex::default()` einsetzen, verschwiege die Fassade genau diese
//!   Lücke, statt sie an den Aufrufer weiterzugeben, der sie (aus
//!   kuratierten Kanten oder `suggest_relations`-Vorschlägen) selbst
//!   schließen muss.
//! - **Nein** — `ByteSpan`: nur nötig, um einen `Chunk` selbst zu *bauen*
//!   (das tut ausschließlich `harw-lens-chunk`, innerhalb von
//!   `harw-lens-source`); ein Aufrufer von [`ask`] liest `chunk.span.start`/
//!   `.end` nur per Feldzugriff, ohne den Typnamen je zu benennen.
//! - **Nein (begründet, nicht nur „fraglich")** — `IndexManifest`: wird
//!   weder von [`build`] noch von [`ask`] zurückgegeben (auch nicht in
//!   [`IndexBuildReport`], das bewusst nur `index_name`, `visibility`,
//!   `chunk_count`, `embedded_count`, `reused_count` trägt). Ein Manifest
//!   zu benennen setzte voraus, dass ein Aufrufer auch `FlatIndex::load`
//!   oder `VectorIndex::search` direkt aufrufen könnte — genau der
//!   Rückfall in die Innenteile, den diese Fassade verhindern soll. Eine
//!   künftige Introspektionsfunktion (z. B. „welches Modell hat Index X
//!   gebaut") wäre eine bewusste neue Ergänzung, kein vergessener Fall.
//! - **Nein** — `CostEstimate`, `CostEstimator`, `BytesOverFour`,
//!   `BudgetSpec`, `Packed`: gehören zu `pack`, dem laut
//!   `harw-lens-types`/`harw-lens-rank`-Dokumentation **eingefrorenen**
//!   Vertrag zwischen Lens und der Kontextmontage (`harw-context` ruft
//!   `harw_lens_rank::pack` direkt auf; AW6-07 prüft das ausdrücklich).
//!   [`ask`] liefert rohe, noch nicht gepackte Treffer — ob und wie sie in
//!   ein Budget gepackt werden, ist die Entscheidung der Kontextmontage
//!   über den bereits fixierten `pack`-Vertrag, nicht eine zweite
//!   Vermittlung durch diese Fassade.
//!
//! ## `harw-lens-rank` — vier reine Funktionen
//! - **Nein**, alle vier (`rrf_fuse`, `mmr`, `collapse`, `pack`):
//!   `collapse` ruft [`ask`] bereits intern auf (über
//!   `harw_lens_query::query`); ein zusätzlicher Export erlaubte einem
//!   Aufrufer, bereits entdoppelte Treffer ein zweites Mal zu entdoppeln
//!   oder die Abfrage-Pipeline von Hand nachzubauen. `rrf_fuse`/`mmr` sind
//!   an dieser Ausbaustufe in keiner Lens-Pipeline verdrahtet (keine
//!   Mehrfach-Retriever-Fusion, keine Diversitäts-Nachsortierung) — sie
//!   jetzt freizugeben wäre, Fähigkeit zu zeigen, die diese Fassade noch
//!   nicht orchestriert. `pack` siehe oben (eingefrorener Lens/Kontext-
//!   Vertrag, direkt von `harw-context` genutzt).
//!
//! ## `harw-lens-chunk` — drei Zerlegungsstrategien
//! - **Nein**, alle (`chunk_markdown`, `chunk_rust`, `chunk_plain`,
//!   `suggest_relations`, `DEFAULT_TARGET_BYTES`, `DEFAULT_OVERLAP_BYTES`):
//!   [`build`] nimmt bereits vollständigen, noch nicht zerlegten Text
//!   entgegen ([`RawDocument::text`]) und zerlegt ihn ausschließlich intern
//!   (`harw-lens-source` ruft `chunk_markdown` auf, versioniert über
//!   [`CHUNKER_VERSION`]). Ein Aufrufer, der selbst zerlegen könnte, könnte
//!   auch doppelt zerlegen oder die inkrementelle Digest-Cache-Zusage von
//!   `harw-lens-source` unterlaufen.
//!
//! ## `harw-lens-store` — Speichermechanik
//! - **Nein**, `LensStore` vollständig (auch nicht einzelne Methoden): der
//!   wichtigste Grund, warum diese Fassade existiert. Ein Aufrufer mit
//!   einem `LensStore`-Handle könnte Chunks/Indizes direkt lesen oder
//!   schreiben — vorbei an genau der physischen Sichtbarkeitstrennung, die
//!   [`build`] (je Sichtbarkeits-Bucket ein eigener Store) und [`ask`]
//!   (Prüfung über [`ReadScope`], bevor ein Store überhaupt geöffnet wird)
//!   durchsetzen.
//!
//! ## `harw-lens-index` — Vektor-/BM25-Index
//! - **Nein**, alle (`VectorIndex`, `Query`, `FlatIndex`, `Bm25Index`):
//!   dieselbe Umgehungsgefahr wie bei `LensStore` — ein direkt gehaltener
//!   `FlatIndex` ließe sich per `.search()` ohne jede `ReadScope`-Prüfung
//!   abfragen. Welche konkrete Indeximplementierung [`ask`] intern
//!   verwendet, ist zudem eine Lens-interne Entscheidung, die sich ändern
//!   können muss, ohne dass Aufrufer-Code sich mitändert.
//!
//! ## `harw-lens-embed` — Katalog, Präfixe, Embedder
//! - **Ja** — [`Embedder`], [`EmbeddingDescriptor`]: unvermeidlich, weil
//!   direkte Parametertypen von [`build`]/[`ask`].
//! - **Ja** — [`EmbeddingCatalog`], [`EmbeddingRole`], [`route`],
//!   [`ModelEntry`], [`RuntimeProfile`]: ein Aufrufer muss vor jedem
//!   [`build`]/[`ask`]-Aufruf entscheiden, welches Modell für welche Art
//!   Inhalt zuständig ist (Code/Prosa/Abfrage/Vertraulich) — ohne diese
//!   Typen müsste er Deskriptoren raten und verlöre insbesondere
//!   [`route`]s Fail-Closed-Garantie (`Confidential` nie an ein entferntes
//!   Modell). [`ModelEntry`]/[`RuntimeProfile`] sind direkte Rückgabetypen
//!   von re-exportierten Methoden/Funktionen ([`EmbeddingCatalog::entry_for_role`],
//!   [`route`]) und deshalb Teil der Fläche, nicht nur ihre Felder.
//! - **Ja** — [`DeterministicEmbedder`]: der einzige Embedder, den diese
//!   Fassade ohne echtes Modell-Backend bedienen kann; nötig, damit
//!   Konsumenten [`build`]/[`ask`] in eigenen Tests deterministisch prüfen
//!   können, ohne selbst von `harw-lens-embed` abzuhängen.
//! - **Nein** — [`prepare_document`], [`prepare_query`]: [`build`] ruft
//!   `prepare_document` bereits an der einen richtigen Stelle auf
//!   (innerhalb von `build_index`), [`ask`] entsprechend `prepare_query`
//!   (innerhalb von `query`). Ein Aufrufer, der sie selbst aufriefe, würde
//!   entweder doppelt präfixieren oder — schlimmer — das falsche Präfix an
//!   der falschen Stelle von Hand schreiben; genau die Verwechslung, vor
//!   der `harw-lens-embed`s eigene Dokumentation warnt.
//! - **Ja (seit diesem Knoten)** — [`RemoteEmbedder`], [`HttpEmbedBackend`],
//!   [`DimensionCheckedEmbedder`]: `harw-lens-embed` trägt seit Knoten AW7-06
//!   mit [`HttpEmbedBackend`] den ersten produktionsreifen, echten
//!   Einbettungs-Transport (siehe dessen Moduldoku). Der erste tatsächliche
//!   Konsument dieser Fassade (`harw-tool-lens`) muss vor jedem
//!   [`ask`]-Aufruf entscheiden, ob er den deterministischen Platzhalter oder
//!   einen echten entfernten Einbetter verwendet — eine Entscheidung, die
//!   laut Kriterium 3 dem Aufrufer gehört, nicht dieser Fassade. Ohne diese
//!   drei Namen könnte `harw-tool-lens` diese Wahl nicht treffen, ohne selbst
//!   an `harw-lens-embed` zu hängen (genau die Umgehung, die eine Fassade
//!   verhindern soll). [`DimensionCheckedEmbedder`] gehört zwingend dazu:
//!   ein `RemoteEmbedder<HttpEmbedBackend>`, dessen entferntes Modell
//!   stillschweigend die falsche Vektorlänge liefert, macht sonst einen
//!   ganzen Index unbrauchbar, ohne dass der Aufrufer beim Einbetten selbst
//!   einen Fehler sähe — der Wrapper macht diese Prüfung zur Aufrufzeit.
//!   **Was weiterhin bewusst draußen bleibt:** `RemoteEmbedBackend` (das
//!   Transport-*Trait*) — kein Code in dieser Fassade oder in
//!   `harw-tool-lens` implementiert einen eigenen Transport; der einzige
//!   benötigte Name ist die fertige Implementierung [`HttpEmbedBackend`]
//!   selbst. Ein Aufrufer, der einen dritten, selbst geschriebenen
//!   `RemoteEmbedBackend` bräuchte, hinge ohnehin direkt von
//!   `harw-lens-embed` ab — das ist keine Lücke, die diese Fassade schließen
//!   müsste, solange kein solcher Aufrufer existiert.
//! - **Nein** — `EmbeddingSpec`, `ObservedBehavior`: nur als Felder von
//!   [`ModelEntry`] erreichbar (`.spec`, `.observed`), nicht als
//!   Rückgabetyp einer re-exportierten Funktion selbst — ein Aufrufer, der
//!   `entry.spec.dimensions` liest, tut das per Feldzugriff, ohne den
//!   Typnamen zu benennen. `ObservedBehavior` ist zudem in dieser
//!   Ausbaustufe „anfangs immer leer" (siehe `harw-lens-embed`s
//!   Moduldokumentation) — Fläche für ein Feature freizugeben, das noch
//!   keine Daten trägt, wäre verfrüht.
//!
//! ## `harw-lens-source` — Quellenbindung, Indexaufbau
//! - **Ja** — [`RawDocument`], [`IndexBuildReport`]: Eingabe- bzw.
//!   Rückgabetyp von [`build`].
//! - **Ja** — [`DOCS_DESIGN_INDEX`], [`KNOWLEDGE_PALACE_INDEX`],
//!   [`DEFAULT_VISIBILITY`], [`OPERATOR_ONLY_VISIBILITY`],
//!   [`CHUNKER_VERSION`]: die Indexnamen und Sichtbarkeits-Bucket-Namen,
//!   die ein Aufrufer für [`IndexSelector`]/[`RawDocument::visibility`]
//!   ohnehin als Zeichenketten bräuchte — als Konstanten re-exportiert,
//!   damit er sie nicht selbst abtippt.
//! - **Ja** — [`visibility_of_scope`], [`collect_design_docs`],
//!   [`collect_palace_documents`]: die drei sanktionierten Wege, um
//!   [`RawDocument`]s zu gewinnen, bevor [`build`] aufgerufen wird. Ohne
//!   sie müsste ein Aufrufer Markdown-Verzeichnisse selbst durchlaufen oder
//!   Palace-Artefakte selbst aus einem `KnowledgeIndex` filtern — genau die
//!   Wiederholung bereits gelöster Logik, die eine Fassade verhindern soll.
//! - **Ja** — [`SourceError`] (re-exportiert, siehe `error.rs`): nötig,
//!   damit ein Aufrufer eine Variante von [`LensError::Source`] per `match`
//!   benennen kann, ohne selbst `harw-lens-source` als Abhängigkeit zu
//!   brauchen.
//! - **Nein** — `build_index` selbst: durch [`build`] ersetzt. Zwei Namen
//!   für dieselbe Funktion wären keine Auswahl, sondern Verwirrung.
//!
//! ## `harw-lens-query` — Abfragepfad
//! - **Ja** — [`IndexSelector`], [`ReadScope`]: Pflichtparameter von
//!   [`ask`]; siehe `ask.rs` für die Begründung, warum `scope` **kein**
//!   `Option` ist.
//! - **Ja** — [`QueryProvenance`]: Pflichtparameter von [`ask`], an
//!   derselben Stelle wie in [`harw_lens_query::query_scoped`] (nach
//!   `descriptor`, vor `edges`). Wie bei [`EdgeIndex`]/[`CollapsePolicy`]
//!   oben ist das Kriterium 3 einschlägig: womit der Aufrufer eingebettet
//!   hat, ist eine Entscheidung, die dem Aufrufer gehört, nicht dieser
//!   Fassade. Würde [`ask`] `provenance` intern still aus dem aufgelösten
//!   Index ableiten, wäre die Prüfung tautologisch — genau der Fehler, der
//!   diesen Knoten ausgelöst hat: die erste Fassung von
//!   `harw_lens_query::query` nahm `index.manifest().clone()` als Manifest
//!   der Abfrage, also das Manifest genau des Index, den sie durchsuchte,
//!   wodurch `IndexManifest::compatible_with` nie fehlschlagen konnte. Siehe
//!   [`harw_lens_query::QueryProvenance`] für die vollständige Begründung.
//! - **Ja** — [`QueryError`] (re-exportiert, siehe `error.rs`): analog zu
//!   [`SourceError`] oben — nötig für `match`-Zugriff auf z. B.
//!   [`QueryError::IndexNotVisible`] ohne eigene Abhängigkeit auf
//!   `harw-lens-query`.
//! - **Nein** — `resolve_index`, `query`: Bausteine von `query_scoped`
//!   (durch [`ask`] ersetzt). `resolve_index` gibt einen `FlatIndex`
//!   zurück, `query` nimmt einen `&dyn VectorIndex` entgegen — beides
//!   Typen, die diese Fassade bewusst nicht freigibt (siehe oben); sie
//!   isoliert freizugeben liefe leer.
//!
//! # Kein `pub use *`
//! Jeder Re-Export oben ist einzeln benannt und einzeln begründet. Ein
//! Glob-Reexport einer der zehn Innencrates würde diese Begründungsarbeit
//! umgehen und dem Konsumenten dieselbe Fläche unter einem anderen Namen
//! zurückgeben — keine Fassade, sondern ein Verzeichnis.
//!
//! # `LensError`: verdichtet und durchgereicht zugleich
//! Siehe die Moduldokumentation von [`LensError`] für die vollständige
//! Begründung. Kurzfassung: genau zwei
//! Varianten ([`LensError::Source`], [`LensError::Query`]) — das ist die
//! Verdichtung an dieser Grenze —, aber jede Variante wickelt den bereits
//! vollständig differenzierten inneren Fehlertyp unverändert ein, statt ihn
//! auf eine Zeichenkette zu reduzieren. Ein Selektor außerhalb des
//! [`ReadScope`] bleibt deshalb als
//! `LensError::Query(QueryError::IndexNotVisible { .. })` unterscheidbar von
//! z. B. `LensError::Query(QueryError::Index(_))`.
//!
//! # Nebenläufigkeit
//! Alle Typen dieser Fassade sind reine Daten oder zustandslose
//! Weiterleitungen. [`build`] und [`ask`] halten keinen Zustand zwischen
//! Aufrufen; sicher aus mehreren Threads parallel aufrufbar, solange
//! `embedder` es selbst ist (`Embedder: Send + Sync`).
//!
//! # Fehler
//! [`LensError`] (Typalias [`LensResult`]) ist der einzige Fehlertyp dieser
//! Crate.
//!
//! # Examples
//! Ein vollständiger Bau- und Abfragedurchlauf über die Fassade:
//!
//! ```rust
//! use harw_lens::{
//!     ask, build, IndexSelector, QueryProvenance, RawDocument, ReadScope, CHUNKER_VERSION,
//!     DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX,
//! };
//! use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
//! use harw_lens_types::{CollapsePolicy, EdgeIndex, Locality, Metric, SourceRef};
//!
//! let home = tempfile::tempdir()?;
//! let embedder = DeterministicEmbedder::new(16);
//! let descriptor = EmbeddingDescriptor {
//!     document_prefix: "passage: ".to_owned(),
//!     query_prefix: "query: ".to_owned(),
//!     normalize: false,
//! };
//! let documents = vec![RawDocument {
//!     source: SourceRef::File { path: "intro.md".to_owned() },
//!     text: "Harwness Fassaden binden zehn Crates unter einem Namen.".to_owned(),
//!     visibility: DEFAULT_VISIBILITY.to_owned(),
//! }];
//!
//! let reports = build(
//!     home.path(),
//!     DOCS_DESIGN_INDEX,
//!     &documents,
//!     "test-model",
//!     Locality::Local,
//!     Metric::Cosine,
//!     &embedder,
//!     &descriptor,
//! )?;
//! assert_eq!(reports[0].chunk_count, 1);
//!
//! let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
//! let scope = ReadScope::single(DEFAULT_VISIBILITY);
//! let provenance = QueryProvenance {
//!     model: "test-model".to_owned(),
//!     chunker_version: CHUNKER_VERSION,
//! };
//! let hits = ask(
//!     home.path(),
//!     &selector,
//!     &scope,
//!     "Fassaden binden Crates",
//!     &embedder,
//!     &descriptor,
//!     &provenance,
//!     &EdgeIndex::default(),
//!     CollapsePolicy::ByDigest,
//!     10,
//! )?;
//! assert_eq!(hits.len(), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Stand
//! Knoten **AW5-10**; Ebene **L5** im Zielgraphen — der einzige Knoten
//! dieser Ebene. Abhängigkeiten: `harw-lens-types`, `harw-lens-embed`,
//! `harw-lens-source`, `harw-lens-query` (alle vorgelagert gelandet) sowie
//! `harw-macros` (für `LensError`).
//!
//! Ein Folgeknoten (nach AW7-06, das [`HttpEmbedBackend`] in
//! `harw-lens-embed` einführte) hat die Fassadenfläche um
//! [`RemoteEmbedder`], [`HttpEmbedBackend`] und [`DimensionCheckedEmbedder`]
//! erweitert (siehe die Begründung im Abschnitt „`harw-lens-embed`" oben) und
//! `harw-tool-lens/src/provenance.rs` so umgebaut, dass es zwischen dem
//! deterministischen Platzhalter und einem über [`HttpEmbedBackend`]
//! angebundenen entfernten Modell wählt — die Vorgabe ohne konfigurierten
//! Endpunkt bleibt dabei unverändert der deterministische Platzhalter. Siehe
//! `harw-tool-lens`s `//!`-Block für die vollständige Begründung, insbesondere
//! warum [`EmbeddingRole::Confidential`] davon unberührt bleibt.

mod ask;
mod build;
mod error;

pub use ask::ask;
pub use build::build;
pub use error::{LensError, LensResult};

// --- harw-lens-types: reines Vokabular (siehe Begründung oben) ---
pub use harw_lens_types::{
    Chunk, ChunkDigest, CollapsePolicy, EdgeIndex, EdgeKind, Locality, Metric, Ranked, SourceRef,
};

// --- harw-lens-embed: Katalog, Präfixe, Embedder (siehe Begründung oben) ---
pub use harw_lens_embed::{
    route, DeterministicEmbedder, DimensionCheckedEmbedder, Embedder, EmbeddingCatalog,
    EmbeddingDescriptor, EmbeddingRole, HttpEmbedBackend, ModelEntry, RemoteEmbedder,
    RuntimeProfile,
};

// --- harw-lens-source: Quellenbindung, Indexaufbau (siehe Begründung oben) ---
pub use harw_lens_source::{
    collect_design_docs, collect_palace_documents, visibility_of_scope, IndexBuildReport,
    RawDocument, SourceError, CHUNKER_VERSION, DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX,
    KNOWLEDGE_PALACE_INDEX, OPERATOR_ONLY_VISIBILITY,
};

// --- harw-lens-query: Abfragepfad (siehe Begründung oben) ---
pub use harw_lens_query::{IndexSelector, QueryError, QueryProvenance, ReadScope};
