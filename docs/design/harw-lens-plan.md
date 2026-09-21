# `harw-lens`: Wissensindex, Vektorspeicher und Retrieval für Harwness

**Status:** Entwurf zur Diskussion, noch nicht normativ
**Zweck:** Ein eigenes Retrieval-Subsystem für Harwness: Chunking,
Embeddings, Vektor- und Lexikalindex, hybride Suche, Relationsauflösung
**Inspiration:** Vectory (eigenständiges Projekt), ausdrücklich als
Ideenquelle, nicht als Vorlage. Was übernommen und was bewusst verworfen
wird, steht in §1.
**Verwandt:** `harw-context-plan.md`, `harw-context-steward-plan.md`,
`harw-dod-integration-and-dependencies.md` (Doktrin D1 bis D9)

---

## 0. Der Name und die Verantwortungsgrenze

**`harw-lens`.** Eine Linse fokussiert, sie besitzt nicht, was sie
betrachtet. Genau das ist die Verantwortungsgrenze: Lens indiziert und
rankt, aber es besitzt kein Wissen. Artefakte gehören `harw-knowledge`,
Signale gehören `harw-memory`, Transkripte gehören `harw-session-store`,
Quellcode gehört dem Workspace. Lens löscht nie etwas, es kennt nur Digests
und Zeiger.

Der Name kodiert die Invariante, wie `harw-dod` für Detect, Orient, Defend.
Die schlichte Alternative wäre `harw-retrieval`; die Entscheidung ist
kosmetisch, die Grenze ist es nicht.

---

## 1. Was von Vectory übernommen wird und was nicht

### 1.1 Übernommen, weil es die guten Ideen sind

**Der Index als inhaltsadressiertes Artefakt mit Manifest.** Ein Index wird
geschrieben, per Digest zurückgelesen und trägt ein Manifest mit Version,
Backend, Dimension und Zeilenzahl. Das passt exakt in Harwness' Welt aus
blake3-Digests und `SnapshotId`: ein Index wird damit reproduzierbar,
vergleichbar und austauschbar. Bestes Einzelstück des Vectory-Entwurfs.

**Der `VectorIndex`-Trait mit austauschbaren Backends.** Flach zuerst, ANN
später, hinter einem Trait. Dieselbe Doktrin wie bei den Providern: eine
Schnittstelle, viele Implementierungen.

**BM25 plus RRF.** Vectory hat eine abhängigkeitsfreie In-Memory-BM25 und
eine Reciprocal-Rank-Fusion, um dichte und lexikalische Ranglisten zu
verschmelzen. Harwness hat bereits ein BM25-artiges Recall in
`harw-knowledge`; die dichte Hälfte und die Fusion fehlen. Genau das ist die
Lücke.

**Reranking mit MMR.** Maximal Marginal Relevance ist der Mechanismus, der
aus "die zehn ähnlichsten" die "zehn nützlich verschiedenen" macht. Für ein
Kontextbudget ist das wichtiger als Präzision.

**Relationsexpansion und, vor allem, Relationskategorien beim Kollabieren.**
Vectory klassifiziert Kanten in semantische Kategorien und entdoppelt
Treffer danach: Cluster-Zugehörigkeit und Provenienz werden zusammengefasst,
bewusste Gegensätze bleiben getrennt. **Das ist die stärkste übertragbare
Idee überhaupt,** weil Harwness diese Kanten längst hat und nicht nutzt:
`superseded_by` im Palace ist Provenienz, der `contradiction_index` ist
Spannung, Topic-Zugehörigkeit ist Cluster. Details in §5.3.

**Chunking als Vertrag mit Vorschlägen.** Vectory lässt den Chunker
Chunk- und Relationsvorschläge liefern statt fertiger Wahrheit. Das ist das
Vorschlagen-statt-Committen-Muster, das Harwness ohnehin überall fährt.

**Externe Verknüpfung ohne Domänenübernahme.** Vectory referenziert fremde
Zeilen über (Quelle, Tabelle, Schlüssel) und weigert sich ausdrücklich zu
verstehen, was diese Daten bedeuten. Für Lens heißt das: Workspace-Dateien
und Dependency-Quellen werden referenziert, nie kopiert und nie
interpretiert.

**Der `EmbeddingProvider`-Trait mit Registry.** Spiegelt exakt Harwness'
Provider-Muster, inklusive lokaler Backends.

### 1.2 Bewusst verworfen

**Postgres-Katalog und Mandantenfähigkeit.** Vectory ist ein
Mehrmandanten-Dienst mit `org_id`, Sqlx-Repositories und RLS. Harwness ist
single-host, dateibasiert, mit `fs4`-Advisory-Locks, `sync_data` und
atomarem Rename. Ein Postgres im Kern wäre ein Betriebsdienst mehr, ein
zweites Rechtemodell und eine Abhängigkeit gegen D1. **Die Mandantenachse
wird durch `VisibilityScope` ersetzt**, das es bereits gibt und das schärfer
ist.

**Python-Worker als Subprozesse.** Vectory beaufsichtigt Python-Prozesse mit
NDJSON-Protokoll fürs Chunking. In Harwness wäre das ein
`ExecuteProcess`-Recht für eine Aufgabe, die reines Rechnen ist. Chunking
wird in Rust gebaut, deterministisch und testbar.

**Eigene HTTP-API und eigener MCP-Namensraum.** Beides existiert:
`harw-operations` mit `Surface`, `harw-mcp-server`, `harw-web`. Lens bekommt
Operationen und ein Agenten-Tool, keinen eigenen Dienst.

**UUID-Schlüssel überall.** Harwness verwendet typisierte Newtypes und
Inhaltsdigests. Ein Chunk hat einen Digest, keine zufällige ID.

**Ein Daemon.** `vectoryd` ist ein Serverprozess. Lens ist eine Bibliothek
plus Jobs auf der vorhandenen Job-Bridge.

---

## 2. Die Lücke, die Lens schließt

`harw-knowledge::recall` ist heute keyword-basiert mit BM25-Parametern
K1 und B, Hop-Decay, und harten Obergrenzen von 256 Artefakten und 8 Hops.
Das ist ein gutes Fundament und gleichzeitig eine Decke:

| Heute | Mit Lens |
|---|---|
| nur lexikalisch | dicht plus lexikalisch, per RRF verschmolzen |
| Index wird zur Laufzeit gebildet | Index ist ein persistiertes, digest-adressiertes Artefakt |
| harte Konstanten 256 und 8 | Budget als Abfrageparameter, gedeckelt durch die `ContextCeiling` |
| Artefakt als kleinste Einheit | Chunk als kleinste Einheit, Artefakt bleibt die Identität |
| Kanten nur zur Traversierung | Kanten auch zum Entdoppeln (Kategorien) |
| nur Knowledge-Artefakte | zusätzlich Workspace-Dateien, Dependency-Quellen, Transkripte |

Der letzte Punkt ist der, der neue Fähigkeiten eröffnet:
`harw-code-graph` lokalisiert bereits Registry-Quellverzeichnisse und baut
docs.rs-URLs. Ein Index über die Quellen der eigenen Abhängigkeiten ist die
Grundlage für einen Analyse-Modus, der Fragen wie "wo im Baum wird das
gemacht" beantworten kann, ohne dass ein Modell raten muss.

---

## 3. Normative Invarianten

Nummernkreis L.

**L1. Lens besitzt nichts.** Es speichert Digests, Zeiger, Vektoren und
Ranglisten. Löschen ist nie Lens' Aufgabe; `superseded_by` wird respektiert,
nicht durchgesetzt.

**L2. Ein Index ist ein Artefakt mit Manifest.** Modell-ID, Dimension,
Metrik, Chunker-Version, Quellmengen-Digest und Backend stehen im Manifest.
Ein Index mit anderem Modell ist ein anderer Index, kein aktualisierter.

**L3. Sichtbarkeit ist eine Indexgrenze, kein Filter.** Physisch getrennte
Indizes je Sichtbarkeitsklasse. Begründung in §4.2.

**L4. `OperatorOnly`-Inhalte werden ausschließlich mit lokalen Embeddings
indiziert.** Ein entfernter Embedding-Aufruf ist eine Übertragung des
Inhalts. Der Provider trägt eine `Locality`, das Manifest zeichnet sie auf,
und der Build lehnt die Kombination fail-closed ab.

**L5. Chunking ist deterministisch und reproduzierbar.** Gleicher Text,
gleiche Chunker-Version, gleiche Chunks mit gleichen Digests. Sonst wäre
inkrementelles Bauen unmöglich.

**L6. Relationsvorschläge werden vorgeschlagen, nicht geschrieben.** Ein
Chunker, der eine Beziehung erkennt, liefert einen Vorschlag, der als
`Provisional` in den Palace geht und review-gated befördert wird.

**L7. Retrieval ist rein.** Ranking, Fusion, Reranking und Kollabieren sind
Funktionen ohne I/O und ohne Systemzeit. Testbar wie `PlanController::reconcile`.

**L8. Kein Chunk-Inhalt in Telemetrie.** Gemessen werden Zahlen, Digests und
Gründe (`Redact`).

---

## 4. Crate-Landschaft

### 4.1 Zuschnitt

| Crate | L | Deps (intern) | Verantwortung |
|---|---|---|---|
| `harw-lens-types` | 0 | `harw-types` | `ChunkId`, `ChunkDigest`, `Embedding`, `Metric`, `Hit`, `IndexId`, `IndexManifest`, `SourceRef`, `Locality` |
| `harw-lens-chunk` | 1 | `harw-lens-types` | Deterministisches Chunking: Markdown, Rust, Klartext; Chunk- und Relationsvorschläge |
| `harw-lens-embed` | 1 | `harw-lens-types`, `harw-model-catalog` | `EmbeddingProvider`-Trait, Registry, lokale und entfernte Backends |
| `harw-lens-index` | 2 | `harw-lens-types` | `VectorIndex`-Trait, `FlatIndex`, `Bm25Index`, Persistenz und Manifest |
| `harw-lens-store` | 2 | `harw-lens-types`, `harw-home` | Inhaltsadressierter Chunk- und Indexspeicher, `fs4`-Lock, atomarer Rename |
| `harw-lens-source` | 2 | `harw-knowledge`, `harw-code-graph`, `harw-session-store`, `harw-sandbox` | Quelladapter, read-only, `ReadScope`-gebunden |
| `harw-lens-query` | 3 | alle darüber | Suchmodi, RRF, MMR, Expansion, Kategorienkollaps, Sichtbarkeitsdurchsetzung |
| `harw-lens` | 4 | Fassade | Reexport, Prelude, keine Logik |
| `harw-tool-lens` | 5 | `harw-lens`, `harw-tools` | Agenten-Tool `lens.search` und `lens.expand` |

Neun Crates, davon zwei Vokabular-Leaves. Kein Daemon, kein Server, keine
Datenbank.

### 4.2 Warum Sichtbarkeit eine Indexgrenze ist (L3)

Der naheliegende Entwurf wäre ein Index über alles plus ein Filter nach
`VisibilityScope` beim Abfragen. Das ist bequem und falsch.

Erstens ist Filtern nach dem Abrufen eine Bug-Klasse: ein vergessener Filter
ist eine Offenlegung, und genau solche vergessenen Prüfungen sind der Grund,
warum Harwness Autorität als Datenstruktur baut statt als Prüfung.

Zweitens leckt Ranking auch dann, wenn der Filter greift. Wie viele Treffer
insgesamt existieren, wie stark die Scores der weggefilterten waren, ob eine
Suche überhaupt etwas findet: das sind Seitenkanäle, die über wiederholte
Abfragen Inhalte rekonstruieren lassen.

Deshalb: **je Sichtbarkeitsklasse ein eigener physischer Index.**
`OperatorOnly` liegt in einem eigenen Index, den ein normaler Agent nicht
öffnet, weil sein `ReadScope` ihn nicht enthält. Die Grenze ist ein Pfad,
kein `if`. Das ist derselbe Gedanke wie beim `ReadScope` der Sensoren.

Der Preis ist Redundanz für Inhalte, die in mehreren Klassen sichtbar sind.
Der ist hinnehmbar: Chunks liegen inhaltsadressiert nur einmal, doppelt sind
nur die Vektoren.

---

## 5. Kernentwurf

### 5.1 Vokabular

```rust
/// Ein Chunk ist durch seinen Inhalt identifiziert, nicht durch eine ID.
pub struct ChunkDigest([u8; 32]);          // blake3, wie SnapshotId

pub struct Chunk {
    pub digest: ChunkDigest,
    pub source: SourceRef,
    pub span: ByteSpan,                     // Position in der Quelle
    pub text: String,
    pub kind: ChunkKind,                    // Prose | Code | Heading | Table | Frontmatter
}

/// Zeiger auf fremdes Eigentum. Lens interpretiert ihn nicht (L1).
pub enum SourceRef {
    Artifact(ArtifactId),                   // harw-knowledge
    WorkspaceFile { path: PathBuf, rev: RepoRevision },
    DependencySource { package: String, version: String, path: PathBuf },
    Transcript { session: SessionId, turn: TurnId },
}

pub struct IndexManifest {
    pub id: IndexId,
    pub backend: &'static str,              // "flat" | "hnsw" | "bm25"
    pub metric: Metric,                     // Cosine | L2 | InnerProduct
    pub dim: usize,
    pub model: EmbeddingModelId,
    pub locality: Locality,                 // Local | Remote  (L4)
    pub chunker_version: u32,               // (L5)
    pub visibility: VisibilityScope,        // (L3)
    pub source_set_digest: [u8; 32],        // was drin ist
    pub n_chunks: usize,
    pub built_at: jiff::Timestamp,
}
```

Das Manifest ist der Grund, warum ein Index austauschbar wird: zwei Indizes
mit unterschiedlichem `model` oder `chunker_version` sind unterschiedliche
Objekte, und eine Abfrage gegen den falschen ist ein Typfehler statt eines
stillen Qualitätsverlusts.

### 5.2 Chunking

Rein, deterministisch, formatbewusst. Drei Strategien, alle in Rust:

**Markdown** entlang der Überschriftenhierarchie, mit Überlappung nur an
Absatzgrenzen, Frontmatter separat. Für die Doku-Vaults und die
Design-Dokumente.

**Rust** entlang syntaktischer Grenzen: Item, `impl`-Block, Modul. Doc-Kommentare
bleiben bei ihrem Item, weil sie zusammen gefunden werden sollen. Ein
Modul-Doc (`//!`) ist ein eigener Chunk mit hoher Gewichtung, weil in diesem
Workspace die Modul-Docs die eigentliche Spezifikation tragen.

**Klartext** mit Absatz- und Satzgrenzen als Rückfallebene.

```rust
pub fn chunk(text: &str, kind: SourceKind, budget: ChunkBudget)
    -> (Vec<Chunk>, Vec<RelationProposal>);
```

Relationsvorschläge entstehen dabei aus dem, was der Chunker ohnehin sieht:
Überschriftenhierarchie ergibt `ChildOf`, Markdown-Links und Rust-Pfade
ergeben `References`, aufeinanderfolgende Chunks ergeben `FollowedBy`. Sie
gehen als `Provisional` in den Palace (L6).

### 5.3 Kategorien-Kollaps, das Herzstück

Vectorys beste Idee, auf Harwness' vorhandene Kanten abgebildet:

| Kategorie | Harwness-Kanten | Verhalten beim Kollabieren |
|---|---|---|
| `ClusterBinding` | Topic-Zugehörigkeit, `ChildOf`, `DerivedFrom` | zusammenfassen, bester Vertreter bleibt |
| `ProvenanceBinding` | `superseded_by` im Palace | zusammenfassen, aktuellste Fassung bleibt |
| `TensionWith` | `contradiction_index`, alle vier Konfliktarten | **niemals zusammenfassen**, beide behalten |
| `Reference` | Links, Symbolverweise | behalten, Rang leicht abwerten |
| `Sequence` | `FollowedBy` | zusammenfassen, wenn benachbart und beide im Budget |

Die dritte Zeile ist die wichtigste und der Grund, warum diese Idee für
Harwness besser passt als für Vectory. Ein Widerspruch ist genau das, was
ein Agent sehen muss, und eine reine Ähnlichkeitssuche würde die beiden
widersprechenden Fassungen als Dubletten behandeln und eine davon
wegwerfen. Der `contradiction_index` existiert bereits mit vier
Konfliktarten; Lens macht ihn erstmals im Retrieval wirksam.

Aus "die zehn ähnlichsten Treffer" wird damit "die zehn nützlich
verschiedenen, mit erhaltenen Spannungen". Für ein knappes Kontextbudget ist
das der eigentliche Gewinn.

### 5.4 Abfrage

```rust
pub struct LensQuery {
    pub text: String,
    pub mode: SearchMode,                  // Dense | Sparse | Hybrid { k_rrf: u32 }
    pub budget: RetrievalBudget,           // top_k, max_chunks, max_tokens, max_hops
    pub sources: Vec<SourceFilter>,
    pub expand: Option<ExpandSpec>,        // Tiefe, Kantentypen
    pub collapse: CollapsePolicy,
    pub rerank: RerankPolicy,              // Identity | Mmr { lambda }
}

/// Rein (L7). Kein I/O, keine Systemzeit.
pub fn rank(dense: &[Hit], sparse: &[Hit], q: &LensQuery) -> Vec<Hit>;
```

`RetrievalBudget` ersetzt die harten Konstanten und wird durch die
`ContextCeiling` des aufrufenden Agenten gedeckelt: ein Agent kann nicht
mehr Kontext holen, als sein Programm zulässt. Damit ist Lens sauber an den
Kontextplan angebunden und nicht ein zweiter Weg an ihm vorbei.

### 5.5 Bauen und inkrementelles Aktualisieren

Weil Chunks inhaltsadressiert sind, ist ein Neubau ein Diff: nur Chunks mit
unbekanntem Digest werden eingebettet. Bei einer Doku-Änderung von zwei
Absätzen kostet das zwei Embedding-Aufrufe, nicht zweitausend.

Der Build ist ein Job auf der vorhandenen Job-Bridge, kein eigener
Scheduler: Plan-Knoten, `MarkReady`, `AdmitJobs`, Job-Evidenz schließt ab.
Genau wie beim Sicherheitssubsystem.

---

## 6. Embeddings und Abhängigkeiten

### 6.1 Der Trait

```rust
pub trait EmbeddingProvider: Send + Sync {
    fn model(&self) -> EmbeddingModelId;
    fn dim(&self) -> usize;
    fn locality(&self) -> Locality;                 // (L4)
    fn embed<'a>(&'a self, texts: &'a [String]) -> ProviderFuture<'a, Vec<Embedding>>;
}
```

`Locality` ist kein Kommentar, sondern der Prüfstein für L4: der
Index-Build lehnt `Locality::Remote` für `VisibilityScope::OperatorOnly` ab,
bevor ein Text das Haus verlässt.

### 6.2 Backends, gegen die Doktrin bewertet

| Backend | Charakter | Doktrin | Urteil |
|---|---|---|---|
| Entfernt über die vorhandenen Provider (OpenAI-kompatibel, Azure, lokale Server über `local-base-url`) | nutzt den HTTP-Client, der schon da ist | D1 erfüllt, keine neue Fläche | **Startpunkt.** Ollama und LM Studio liefern lokale Embeddings über eine HTTP-Schnittstelle und sind damit `Locality::Local`, ohne eine ML-Abhängigkeit zu ziehen |
| In-Prozess über Candle | reines Rust, HuggingFace-Ökosystem, unterstützt BERT-artige Encoder, keine PyTorch- oder Libtorch-Abhängigkeit | D1 erfüllt | **Ziel für echten Offline-Betrieb**, wenn kein lokaler Server laufen soll |
| In-Prozess über ONNX Runtime (`ort`, worauf FastEmbed-rs aufsetzt) | schnell und reif, aber Bindung an eine C-Bibliothek | **verletzt D1 und D2** | nur hinter einem optionalen Feature, niemals im DoD-Pfad |

**Empfehlung:** zuerst gar keine ML-Abhängigkeit. Ein lokaler
Embedding-Server über `local-base-url` deckt `Locality::Local` vollständig
ab, kostet null neue Crates und nutzt den Providerkatalog, der bereits 22
Anbieter kennt. In-Prozess-Inferenz kommt später und dann pur, über Candle,
hinter einem Feature. ONNX bleibt die pragmatische Ausweichoption für
Nicht-Sicherheitskontexte, mit klarer Kennzeichnung im Inventar.

Das ist dieselbe Bewegung wie bei OTel: die schwere Abhängigkeit lebt hinter
dem Trait, der Kern kennt sie nicht.

### 6.3 Index-Backends

`FlatIndex` zuerst, brute force, exakt, ohne Abhängigkeit. Bei den Größen,
um die es hier geht (Doku-Vault, Workspace, Transkripte), sind das
Zehntausende bis wenige Hunderttausend Chunks, und eine flache Suche über
384 oder 768 Dimensionen ist dafür schnell genug, wenn sie ordentlich
geschrieben ist.

ANN erst, wenn eine Messung es verlangt. Dann als zweites Backend hinter
demselben Trait, mit eigenem `IndexManifest`-Eintrag im Feld `backend`, und
mit einer HNSW-Implementierung in reinem Rust oder selbst gebaut. Nicht
vorher: ein ANN-Index ohne Not ist zusätzliche Komplexität und eine
Näherung, wo eine exakte Antwort verfügbar war.

---

## 7. Anbindung an das bestehende System

| Bestehendes Element | Anbindung |
|---|---|
| `harw-knowledge::recall` | bleibt als schneller lexikalischer Pfad; Lens wird die zweite, reichere Route. Die Caps 256 und 8 werden zu Voreinstellungen des `RetrievalBudget` |
| `harw-knowledge::palace` | Quelle für Kanten und `superseded_by`; Ziel für Relationsvorschläge als `Provisional` |
| `harw-memory::contradiction_index` | Quelle für `TensionWith`, damit Widersprüche das Kollabieren überleben |
| `harw-code-graph` | Quelle für Workspace-Struktur und Dependency-Quellverzeichnisse |
| `harw-context` | `RetrievalBudget` wird durch die `ContextCeiling` gedeckelt; Lens-Treffer werden Fragmente mit `TrustClass::Data` |
| `harw-plan-bridge` | Index-Builds laufen als Plan-Knoten über die Job-Bridge |
| `harw-tools` | `harw-tool-lens` mit `lens.search` und `lens.expand`, minimales Schema, `strict` |
| `harw-ops` | `/index` zum Bauen und Inspizieren, `OperationDomain::Knowledge` |
| `harw-web` | Index-Übersicht, Manifeste, Trefferinspektion in der Verwaltungsfläche |
| `harw-observe` | Metriken je Index: Chunkzahl, Bauzeit, Trefferquote, Kollapsrate |

**Was Agenten dürfen:** suchen und expandieren. **Was sie nicht dürfen:**
Indizes bauen, löschen oder Sichtbarkeit ändern. Das sind Operationen mit
`PermissionTier`, keine Tools.

---

## 8. Stufen

**S1. Vokabular.** `harw-lens-types` mit Digest, Manifest, `SourceRef`,
`Metric`, `Locality`. Leaf, ohne Seiteneffekt.

**S2. Chunking.** `harw-lens-chunk` mit den drei Strategien, goldene Tests
über Fixtures aus dem eigenen Repo. Determinismus als Property-Test (L5).

**S3. Speicher und Index.** `harw-lens-store` mit derselben
Persistenzdisziplin wie `harw-session-store`; `harw-lens-index` mit
`FlatIndex`, `Bm25Index`, Manifest und Persistenz.

**S4. Embeddings.** `harw-lens-embed` mit dem Trait und dem
HTTP-Backend über den vorhandenen Providerkatalog. `Locality`-Prüfung
scharf.

**S5. Quellen.** `harw-lens-source` für Knowledge-Artefakte zuerst, danach
Workspace-Dateien. Erster echter Index, erste Messung.

**S6. Abfrage.** `harw-lens-query` mit Dense, Sparse, RRF, MMR, Expansion,
Kategorien-Kollaps. Reine Funktionen, goldene Ranglisten.

**S7. Anbindung.** `harw-tool-lens`, `/index`-Operation, Kontextfragmente,
Budget-Deckelung durch die Ceiling.

**S8. Erweiterung.** Dependency-Quellen und Transkripte als Quellen, Candle
als optionales In-Prozess-Backend, ANN nur bei gemessenem Bedarf.

---

## 9. Prüfungen

**Determinismus des Chunkings.** Gleicher Input, gleiche Digests, über
Property-Tests und einen goldenen Korpus aus dem eigenen Repo.

**Manifest-Disziplin.** Eine Abfrage gegen einen Index mit anderem Modell
oder anderer Chunker-Version wird abgelehnt, nicht stillschweigend
beantwortet.

**L4 fail-closed.** Ein Build über `OperatorOnly`-Inhalte mit einem
`Remote`-Provider schlägt fehl, bevor ein Text die Maschine verlässt.
Nullzähler `lens_remote_embed_on_operator_only`.

**L3 als Pfadgrenze.** Ein Test, dass ein Agent ohne den passenden
`ReadScope` den `OperatorOnly`-Index nicht öffnen kann, auch nicht lesend.

**Kollaps-Fixtures.** Ein Korpus mit einem echten Widerspruchspaar aus dem
`contradiction_index`: beide Treffer müssen überleben. Ein Paar mit
`superseded_by`: nur die aktuelle Fassung überlebt.

**Reinheit.** `rank` ohne I/O und ohne Zeit, Property-Test über
Permutationen der Eingabereihenfolge.

**Inkrementalität.** Zwei Builds nacheinander mit einer geänderten Datei:
die Zahl der Embedding-Aufrufe entspricht der Zahl geänderter Chunks.

---

## 10. Offene Entscheidungen

1. **Name.** `harw-lens` gegen `harw-retrieval`. Die Linse kodiert die
   Besitzgrenze, der schlichte Name ist selbsterklärend.
2. **BM25 doppelt.** `harw-knowledge` hat bereits ein BM25-artiges Recall.
   Entweder Lens bringt ein eigenes mit (Doppelung) oder das vorhandene
   wandert nach `harw-lens-index` und Knowledge konsumiert es. Tendenz: das
   zweite, aber erst in S6, damit S1 bis S5 nichts Bestehendes brechen.
3. **Chunkgröße und Überlappung.** Erst nach den ersten Messungen aus S5
   festzulegen. Vorher geraten wäre wertlos.
4. **Transkripte als Quelle.** Sie sind die größte Datenmenge und die mit
   dem geringsten Signal-Rausch-Verhältnis. Vorschlag: zunächst nur
   Diary-Einträge und Zusammenfassungen indizieren, Rohtranskripte erst,
   wenn ein konkreter Bedarf gemessen ist.
5. **Verhältnis zum Context Steward.** Lens liefert Kontextfragmente,
   der Steward optimiert Kontextprogramme. Offen ist, ob der Steward auch
   `RetrievalBudget`-Vorschläge machen darf. Tendenz: ja, aber unter
   denselben Leitplanken CS1 bis CS7.
