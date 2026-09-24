//! Quellenbindungen der Indizes: welche Dateien/Artefakte zu welchem Index
//! gehören, wie sie zerlegt werden, und wie ein Index inkrementell wächst.
//!
//! Seit einem späteren Knoten (Schließung der Produktionslücke „nichts baut
//! je einen Index") besitzt diese Crate zusätzlich [`index_status`]/
//! [`IndexStatus`] (`crate::status`): die rein lesende Introspektion eines
//! bereits gebauten Index, erster Konsument `harw lens status` in
//! `harw-cli`. Siehe `crate::status`s eigenen `//!`-Block für die volle
//! Begründung.
//!
//! # Verantwortungsbereich
//! Diese Crate bindet vier Quellenarten an vier Indizes (zwei aus Knoten
//! AW5-08, zwei neu aus Knoten AW7-05):
//!
//! - [`DOCS_DESIGN_INDEX`] — die Planungs- und Architekturdokumente,
//!   eingelesen von der Platte über [`collect_design_docs`] und zerlegt mit
//!   `harw_lens_chunk::chunk_markdown`.
//! - [`KNOWLEDGE_PALACE_INDEX`] — die Artefakte des Memory Palace
//!   (`harw_knowledge::ArtifactKind::PalaceNode`), erfasst über
//!   [`collect_palace_documents`] aus einem bereits aufgebauten
//!   `harw_knowledge::KnowledgeIndex` und ebenfalls mit `chunk_markdown`
//!   zerlegt (Palace-Bodies sind Markdown-Prosa).
//! - [`KNOWLEDGE_DIARY_INDEX`] — Diary-Einträge desselben
//!   `harw_knowledge::KnowledgeIndex`
//!   (`harw_knowledge::ArtifactKind::DiaryEntry`), erfasst über
//!   [`collect_diary_documents`], ebenfalls Markdown-Prosa, ebenfalls mit
//!   `chunk_markdown` zerlegt.
//! - [`CODE_RUST_INDEX`] — der Rust-Quelltext dieses Workspace, erfasst über
//!   [`collect_rust_sources`] und mit `harw_lens_chunk::chunk_rust` zerlegt
//!   (Elementgrenzen statt Überschriften).
//!
//! # Die restlichen Indizes (Knoten AW7-05): welche und warum nicht mehr
//! Der Auftrag nennt vier Kandidatenquellen: Plan, Sitzungen,
//! Sicherheitsbefunde, Code. Geprüft gegen das, was der Baum tatsächlich
//! hergibt:
//!
//! - **Plan → kein neuer Index.** `docs/design/build-history.md` ist eine
//!   Markdown-Datei unterhalb des von [`collect_design_docs`] durchsuchten
//!   Wurzelverzeichnisses und damit bereits Teil von [`DOCS_DESIGN_INDEX`] —
//!   dessen eigene Beschreibung nennt ausdrücklich „Planungs- **und**
//!   Architekturdokumente". Ein zweiter Index für dieselbe Datei wäre
//!   Duplikation, keine neue Abdeckung.
//! - **Sitzungen → [`KNOWLEDGE_DIARY_INDEX`].** `harw_knowledge::ArtifactKind`
//!   trägt `DiaryEntry` als eigene, bereits gelandete Variante, strukturell
//!   identisch zu `PalaceNode` (Frontmatter mit `VisibilityScope`,
//!   Markdown-Body). [`collect_diary_documents`] ist deshalb wörtlich
//!   [`collect_palace_documents`] mit vertauschtem Filter — keine neue
//!   Annahme, keine neue Mechanik, dieselbe Sichtbarkeitstrennung über
//!   [`visibility_of_scope`].
//! - **Sicherheitsbefunde → bewusst kein neuer Index.** Zum Zeitpunkt dieses
//!   Knotens gibt es keinen landeten, iterierbaren Bestand aller
//!   DoD-Befunde, der einem `harw_knowledge::KnowledgeIndex::iter` oder
//!   einem Verzeichnis-Walk entspräche: `harw-dod-rules::Finding<S>` ist ein
//!   Typestate-Wert, der bei der Regelauswertung entsteht und (laut eigener
//!   Moduldoku) bewusst **nicht** von außerhalb seiner Crate konstruierbar
//!   ist, und `harw-plan-bridge::FindingStore` legt Recherche-Ergebnisse ab
//!   (`ResearchFinding`), keine Sicherheitsbefunde. Ein Index, der einen
//!   solchen Bestand voraussetzt, müsste diesen Bestand zuerst erfinden —
//!   das wäre eine Annahme über eine noch nicht existierende Persistenz,
//!   keine Ableitung aus etwas Gelandetem. Dieser Knoten baut deshalb
//!   **keinen** `security.findings`-Index; siehe „Später fällig" in
//!   `docs/design/build-history.md`, sobald `harw-dod-escalate`/`harw-dod-warden` einen
//!   iterierbaren Befund-Bestand liefern.
//! - **Code → [`CODE_RUST_INDEX`].** `harw-lens-chunk` trägt mit
//!   `chunk_rust` bereits eine dritte, code-spezifische Zerlegungsstrategie
//!   ohne Konsumenten; [`collect_rust_sources`] liefert die fehlende
//!   Quellenerfassung dafür (rekursiver `.rs`-Walk, symlink-sicher wie
//!   [`collect_design_docs`], `target/`-Verzeichnisse ausgeschlossen).
//!
//! Damit bleibt `harw_lens_chunk::chunk_plain` weiterhin ohne Konsumenten in
//! diesem Knoten — keine der vier Quellen ist unstrukturierter Fließtext
//! ohne Markdown- oder Rust-Elementgrenzen. Das wird hier festgehalten,
//! nicht verschwiegen: **drei** Indizes wurden aus vier Kandidaten
//! gerechtfertigt, nicht vier — „der Vollständigkeit halber" ist keine
//! Begründung, die dieser Knoten für einen fünften oder sechsten Index
//! gelten lässt.
//!
//! Für jeden neuen Index gilt dieselbe physische Sichtbarkeitstrennung wie
//! für die beiden bestehenden (siehe unten): [`KNOWLEDGE_DIARY_INDEX`] kann,
//! wie [`KNOWLEDGE_PALACE_INDEX`], sowohl `workspace`- als auch
//! `operator-only`-Diary-Einträge enthalten (Frontmatter-abhängig);
//! [`CODE_RUST_INDEX`] erhält ausschließlich [`DEFAULT_VISIBILITY`] —
//! Quelltext dieses Repositoriums ist keine `operator-only`-Kategorie nach
//! [`visibility_of_scope`]. Bezüglich Angreiferkontrolle: Design-/Diary-Text
//! stammt aus `harw_knowledge`-Artefakten, die von Agenten geschrieben
//! werden — dieselbe Vertrauensklasse wie Palace-Knoten, nicht höher oder
//! niedriger; Rust-Quelltext stammt von der Platte dieses Workspace und
//! unterliegt derselben Annahme wie `docs.design` (vom Betreiber
//! kontrolliertes Repository, kein Modell-Ausgabe-Pfad). Zur
//! Einbettungsschicht: kein Aufrufer dieser Crate darf für einen
//! `operator-only`-Bucket einen Embedder mit
//! [`harw_lens_types::Locality::Remote`] übergeben — `crate::build`s
//! Moduldoku beschreibt den dafür zuständigen Guard und Nullzähler im
//! Detail; das gilt unverändert für [`KNOWLEDGE_DIARY_INDEX`], den einzigen
//! neuen Index, der überhaupt `operator-only`-Material führen kann.
//!
//! [`build_index`] ist die einzige Stelle, die aus diesen Quellen tatsächlich
//! einen physischen [`harw_lens_index::FlatIndex`] baut und über
//! [`harw_lens_store::LensStore`] ablegt. Diese Crate tut selbst kein
//! Ranking, kein Kollabieren und keine Relevanzbewertung — das ist
//! `harw-lens-query`s (und, für Kollaps, `harw-lens-rank`s) Aufgabe.
//! [`harw_lens_chunk::suggest_relations`] steht Aufrufern, die
//! heuristisch vorgeschlagene `References`-Kanten wollen, weiterhin direkt
//! zur Verfügung; das Bauen eines `EdgeIndex` aus kuratierten Kanten (z. B.
//! `superseded_by`) bleibt bewusst außerhalb dieser Crate — siehe den
//! `# Was diese Crate nicht tut`-Abschnitt unten.
//!
//! # Warum Sichtbarkeit ein getrennter physischer Index ist, kein Filter
//! Das ist die wichtigste Festlegung dieses Knotens. Ein Filter auf einem
//! gemeinsamen Index ist eine Zeile Code, die man vergessen kann — an genau
//! einer von mehreren Stellen, an denen ein Kandidat den Index verlässt
//! (direkter Treffer, Rückverweis-Traversierung, ein später hinzugefügter
//! zweiter Lesepfad, …). Vergisst man sie an einer einzigen Stelle, ist die
//! Folge stiller Datenabfluss: ein Artefakt, das niemand außerhalb des
//! Operator-Kreises sehen sollte, taucht in einem Treffer auf, ohne dass
//! irgendetwas einen Fehler meldet. Genau dieser Fehlerklasse ist in
//! `harw-knowledge` bereits einmal passiert: die Sichtbarkeitsprüfung griff
//! am direkten Kandidatenfilter, aber nicht bei der Rückverweis-Traversierung
//! — eingeschränkte Artefakte rutschten durch.
//!
//! Zwei physisch getrennte Indizes sind eine Struktur, die man nicht
//! vergessen kann: was nicht im Index steht, kann nicht gefunden werden,
//! unabhängig davon, über wie viele verschiedene Lesepfade später gesucht
//! wird. [`build_index`] setzt das um, indem es Dokumente nach
//! `RawDocument::visibility` gruppiert und **je Gruppe einen eigenen**
//! [`harw_lens_store::LensStore`] öffnet, gewurzelt unter
//! [`harw_home::paths::visibility_index_dir`] — der Sichtbarkeitsname steht
//! als eigene Pfadkomponente im Ergebnis, nicht als Feld in einem
//! gemeinsamen Datensatz. [`visibility_of_scope`] entscheidet, welcher
//! Bucket ein `harw_knowledge`-Artefakt anhand seiner
//! `harw_knowledge::VisibilityScope` erhält.
//!
//! # Inkrementalität
//! `harw-lens-chunk`s Zerlegungsfunktionen sind deterministisch: gleicher
//! Input, gleiche [`harw_lens_types::ChunkDigest`]s, immer. [`build_index`]
//! nutzt genau das: das Zerlegen läuft bei jedem Aufruf über das gesamte
//! übergebene Material (billig), aber das **Einbetten** — der teure Teil —
//! wird über [`harw_lens_store::LensStore::has_chunk`] und einen zusätzlichen,
//! digest-adressierten Embedding-Cache auf tatsächlich neue Chunks
//! beschränkt. Ein zweiter Build über unverändertes Material ruft den
//! übergebenen `Embedder` **kein einziges Mal** auf; eine Änderung an einer
//! Quelle führt dazu, dass genau die davon betroffenen Chunks neu eingebettet
//! werden. Siehe die Moduldokumentation von [`build_index`] für die
//! vollständige Mechanik und die zugehörigen Tests für den Beleg (Zählung der
//! tatsächlichen Embedder-Aufrufe).
//!
//! # Was diese Crate nicht tut
//! Sie leitet **keinen** [`harw_lens_types::EdgeIndex`] automatisch aus
//! `harw_knowledge`-Daten ab (etwa aus
//! `harw_knowledge::memory::palace::PalaceNode::superseded_by`): zum
//! Zeitpunkt dieses Knotens gibt es keine öffentliche Bindung zwischen
//! `PalaceNode` und dem tatsächlich über `harw_knowledge::KnowledgeStore`
//! persistierten `KnowledgeArtifact`/`Frontmatter`-Format (`PalaceNode` wird
//! in `harw-knowledge` bislang unabhängig von der Frontmatter-Persistenz
//! de-/serialisiert). Eine `EdgeIndex`-Konstruktion aus einer noch nicht
//! feststehenden Bindung zu raten wäre eine Annahme, keine Ableitung. Ein
//! `EdgeIndex` ist deshalb ein expliziter Parameter des Abfragepfads
//! (`harw-lens-query`), den ein Aufrufer (z. B. die Fassade aus AW5-10) aus
//! kuratierten Kanten, aus `suggest_relations` oder aus einer künftigen
//! `PalaceNode`-Bindung befüllt.
//!
//! # Nebenläufigkeit
//! [`RawDocument`] und [`IndexBuildReport`] sind reine Daten ohne interne
//! Veränderlichkeit. [`build_index`] hält keinen Zustand zwischen Aufrufen;
//! parallele Aufrufe für unterschiedliche `(home, index_name, visibility)`-
//! Tripel sind sicher, gleichzeitige Aufrufe für dasselbe Tripel
//! serialisieren sich über die `fs4`-Advisory-Lock von `harw-lens-store`.
//!
//! # Fehler
//! [`SourceError`] (Typalias [`SourceResult`]) ist der einzige Fehlertyp
//! dieser Crate.
//!
//! # Examples
//! ```rust,no_run
//! use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
//! use harw_lens_types::{Locality, Metric};
//! use harw_lens_source::{build_index, collect_design_docs, DOCS_DESIGN_INDEX};
//!
//! let home = tempfile::tempdir()?;
//! let docs_root = tempfile::tempdir()?;
//! std::fs::write(docs_root.path().join("intro.md"), "# Intro\n\nText.\n")?;
//!
//! let documents = collect_design_docs(docs_root.path())?;
//! let descriptor = EmbeddingDescriptor {
//!     document_prefix: "passage: ".to_owned(),
//!     query_prefix: "query: ".to_owned(),
//!     normalize: false,
//! };
//! let embedder = DeterministicEmbedder::new(16);
//! build_index(
//!     home.path(),
//!     DOCS_DESIGN_INDEX,
//!     &documents,
//!     "test-model",
//!     Locality::Local,
//!     Metric::Cosine,
//!     &embedder,
//!     &descriptor,
//! )?;
//! // Zweiter Build über unverändertes Material: der Embedder wird für
//! // keinen der Chunks erneut aufgerufen (siehe `build_index`-Dokumentation).
//! let second_reports = build_index(
//!     home.path(),
//!     DOCS_DESIGN_INDEX,
//!     &documents,
//!     "test-model",
//!     Locality::Local,
//!     Metric::Cosine,
//!     &embedder,
//!     &descriptor,
//! )?;
//! assert_eq!(second_reports[0].embedded_count, 0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Stand
//! Knoten **AW5-08**; Ebene **L4** im Zielgraphen. Abhängigkeiten
//! `harw-lens-types`, `harw-lens-chunk`, `harw-lens-store`, `harw-lens-index`,
//! `harw-lens-embed` (alle vorgelagert gelandet), `harw-home` (AW0-02, für
//! [`visibility_of_scope`]s Zielpfad) und `harw-knowledge` (parallel
//! gelandet). **Kein Zyklus**: `harw-knowledge/Cargo.toml` hängt an keiner
//! `harw-lens-*`-Crate (geprüft zum Zeitpunkt dieses Knotens).
//!
//! Knoten **AW7-05** fügte [`KNOWLEDGE_DIARY_INDEX`], [`CODE_RUST_INDEX`]
//! und den Nullzähler [`LENS_REMOTE_EMBED_ON_OPERATOR_ONLY`] hinzu, dazu die
//! neue Abhängigkeit `harw-observe` (Mechanik seit AW1-07; dieser Knoten
//! führt den ersten konkreten Zähler dieser Crate ein). Siehe `crate::build`s
//! Moduldoku für den Guard, der diesen Zähler speist.

mod build;
mod document;
mod error;
mod status;
#[cfg(test)]
mod test_support;

pub use build::{IndexBuildReport, LENS_REMOTE_EMBED_ON_OPERATOR_ONLY, build_index};
#[allow(deprecated)]
pub use document::collect_rust_sources;
pub use document::{
    CODE_SOURCE_EXTENSIONS, CODE_SOURCE_MAX_BYTES, CodeChunker, RawDocument, SKIPPED_SOURCE_DIRS,
    chunker_for_path, collect_code_sources, collect_design_docs, collect_diary_documents,
    collect_palace_documents, visibility_of_scope,
};
pub use error::{SourceError, SourceResult};
pub use status::{IndexStatus, index_status};

/// Name des Index für Planungs- und Architekturdokumente
/// ([`collect_design_docs`], zerlegt mit `chunk_markdown`).
pub const DOCS_DESIGN_INDEX: &str = "docs.design";

/// Name des Index für die Artefakte des Memory Palace
/// ([`collect_palace_documents`]).
pub const KNOWLEDGE_PALACE_INDEX: &str = "knowledge.palace";

/// Name des Index für Diary-Einträge des Memory Palace
/// ([`collect_diary_documents`], zerlegt mit `chunk_markdown`; Knoten
/// AW7-05). Siehe den `# Die restlichen Indizes`-Abschnitt der
/// Moduldokumentation für die Begründung.
pub const KNOWLEDGE_DIARY_INDEX: &str = "knowledge.diary";

/// Name des Index für den Rust-Quelltext dieses Workspace
/// ([`collect_rust_sources`], zerlegt mit `harw_lens_chunk::chunk_rust` --
/// siehe `crate::build`s Moduldoku für die indexnamenbasierte
/// Strategie-Auswahl; Knoten AW7-05).
pub const CODE_RUST_INDEX: &str = "code.rust";

/// Sichtbarkeits-Bucket für gewöhnlich lesbares Material — der Standardfall
/// für jede [`harw_knowledge::VisibilityScope`] außer `OperatorOnly` (siehe
/// [`visibility_of_scope`]).
pub const DEFAULT_VISIBILITY: &str = "workspace";

/// Sichtbarkeits-Bucket für ausschließlich operatorlesbares Material
/// (`harw_knowledge::VisibilityScope::OperatorOnly`).
pub const OPERATOR_ONLY_VISIBILITY: &str = "operator-only";

/// Version der Zerlegungsstrategie, mit der [`build_index`] `chunk_markdown`
/// aufruft.
///
/// # Description
/// Wird in jedes gebaute [`harw_lens_types::IndexManifest`] geschrieben und
/// als Teil des Schlüssels für den internen Embedding-Cache verwendet (siehe
/// `crate::build`). Eine künftige Änderung daran, *wie* diese Crate zerlegt
/// (z. B. ein Wechsel von `chunk_markdown` auf eine andere Strategie, oder
/// veränderte Vor-/Nachbearbeitung des Textes vor dem Zerlegen) muss diese
/// Zahl erhöhen: eine Abfrage mit einer älteren `chunker_version` wird sonst
/// stillschweigend gegen Chunks beantwortet, die nach anderen Regeln
/// entstanden sind. `harw-lens-chunk` selbst versioniert seine
/// Zerlegungsfunktionen nicht separat — diese Konstante gehört der
/// *Verwendung* der Funktion durch diese Crate, nicht der Funktion selbst.
pub const CHUNKER_VERSION: u32 = 1;
