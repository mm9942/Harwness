//! Retrieval- und Rangvokabular.
//!
//! # Verantwortungsbereich
//! Besitzt `Chunk`, `SourceRef`, `ByteSpan`, `ChunkDigest`, `IndexManifest`,
//! `Locality`, `Metric` **und** die Rangtypen `Ranked`, `CostEstimate`,
//! `CostEstimator`, `BytesOverFour`, `BudgetSpec`, `CollapsePolicy`,
//! `EdgeKind`, `EdgeIndex` und `Packed`. Die Rangtypen liegen hier und nicht
//! in `harw-context`, weil `pack` aus `harw-lens-rank` sie nennt und
//! `harw-context` `pack` aufruft — lägen sie in `harw-context`, definierten
//! Kontext und Lens einander gegenseitig.
//!
//! - [`chunk`]: Herkunft, Byte-Bereiche und Chunks selbst.
//! - [`manifest`]: was ein Vektorindex über sich behauptet.
//! - [`rank`]: Rangkandidaten, Kostenschätzung, Budgets, Kanten, `pack`-Ergebnis.
//! - [`error`]: der eine Fehlertyp dieser Crate.
//!
//! `#![forbid(unsafe_code)]` kommt bereits aus `[workspace.lints]`
//! (`docs/aw-contract-master.md`) und wird hier nicht erneut gesetzt.
//!
//! # Nebenläufigkeit
//! Alle exportierten Typen sind reine Daten; [`CostEstimator`] ist
//! `Send + Sync` und wird über `&dyn CostEstimator` aus mehreren Threads
//! aufgerufen.
//!
//! # Fehler
//! [`LensTypesError`] (Typalias [`LensTypesResult`]) ist der einzige
//! Fehlertyp dieser Crate.
//!
//! # Examples
//! ```rust
//! use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};
//! use harw_types::ContentDigest;
//!
//! let span = ByteSpan::new(0, 5).expect("start <= end");
//! let chunk = Chunk {
//!     digest: ChunkDigest(ContentDigest::of(b"hello")),
//!     source: SourceRef::File { path: "a.txt".to_owned() },
//!     span,
//!     text: "hello".to_owned(),
//! };
//! assert!(chunk.span.validate(&chunk.text).is_ok());
//! ```
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament). Der Inhalt entsteht in
//! Knoten **AW0-08**; Ebene **L1** im Zielgraphen.

pub mod chunk;
pub mod error;
pub mod manifest;
pub mod rank;

pub use chunk::{ByteSpan, Chunk, ChunkDigest, SourceRef};
pub use error::{LensTypesError, LensTypesResult};
pub use manifest::{IndexManifest, Locality, Metric};
pub use rank::{
    BudgetSpec, BytesOverFour, CollapsePolicy, CostEstimate, CostEstimator, EdgeIndex, EdgeKind,
    Packed, Ranked,
};
