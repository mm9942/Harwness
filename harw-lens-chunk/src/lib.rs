//! Zerlegung von Text in Chunks — drei Strategien, deterministisch.
//!
//! # Verantwortungsbereich
//! Besitzt [`chunk_markdown`], [`chunk_rust`] und [`chunk_plain`] — die drei
//! Zerlegungsstrategien dieses Ausbauprogramms — sowie [`suggest_relations`],
//! das Relationen zwischen Chunks **vorschlägt**, ohne sie zu schreiben.
//! Diese Crate tut kein I/O: Sie bekommt bereits eingelesenen Text und eine
//! [`SourceRef`](harw_lens_types::SourceRef) zur Beschriftung; das Einlesen von Dateien lebt anderswo
//! (`harw-lens-source`, nachgelagert). Sie schreibt auch keinen
//! `EdgeIndex` — das Vertrauen in eine automatisch vorgeschlagene Kante ist
//! Sache des Aufrufers, siehe [`suggest_relations`].
//!
//! Alle vier Funktionen sind **deterministisch**: gleicher Input, gleiche
//! Digests — immer. Keine Systemzeit, keine Zufallsquelle, keine
//! Hashmap-Iterationsreihenfolge (die Chunker verwenden ausschließlich
//! `Vec`, nie `HashMap`/`HashSet` für Reihenfolge-relevante Daten), keine
//! Pfadauflösung. Das ist die erste der sechs Prüfungen, die später jedes
//! Fixture erbt, und der Grund, warum ein Index inkrementell neu gebaut
//! werden kann: nur was sich geändert hat (anderer Digest), wird neu
//! eingebettet. Jede Strategie hat dafür einen eigenen Eigenschaftstest
//! (`test_*_is_deterministic_across_repeated_runs`) über mehrere Eingaben,
//! nicht nur ein Beispiel.
//!
//! - `markdown`: Zerlegung an Überschriftsgrenzen, Codeblöcke bleiben ganz.
//! - `rust_code`: Zerlegung an Elementgrenzen, Modulkopf zuerst.
//! - `plain`: gleitendes Fenster mit Überlappung, wort-/absatzgrenzenweise.
//! - `relations`: Relationsvorschläge zwischen Chunks.
//! - `util`: interne Hilfsfunktionen, gemeinsam genutzt von allen drei
//!   Strategien (kein Teil der öffentlichen Fläche dieser Crate; alle vier
//!   Module oben sind `mod`, nicht `pub mod` — ihre Funktionen werden am
//!   Crate-Root re-exportiert, siehe unten).
//!
//! `#![forbid(unsafe_code)]` kommt bereits aus `[workspace.lints]`
//! (`docs/aw-contract-master.md`) und wird hier nicht erneut gesetzt.
//!
//! # Nebenläufigkeit
//! Alle vier öffentlichen Funktionen sind reine Funktionen ohne interne
//! Veränderlichkeit; sicher aus mehreren Threads parallel für
//! unterschiedliche Eingaben aufrufbar. Keine der Funktionen hält
//! Zustand zwischen zwei Aufrufen.
//!
//! # Fehler
//! Keine dieser vier Funktionen gibt ein `Result` zurück. Jede Eingabe —
//! auch leerer Text oder entartete Parameter wie `target_bytes == 0` in
//! [`chunk_plain`] — ist gültig; entartete Parameter werden intern geklemmt
//! (siehe deren Dokumentation), nicht zurückgewiesen. Diese Crate definiert
//! deshalb keinen eigenen Fehlertyp.
//!
//! # Examples
//! ```rust
//! use harw_lens_chunk::{chunk_markdown, suggest_relations};
//! use harw_lens_types::SourceRef;
//!
//! let source = SourceRef::File { path: "README.md".to_owned() };
//! let text = "# Titel\n\nText.\n\n## Abschnitt\n\nMehr Text.\n";
//!
//! let chunks = chunk_markdown(&source, text);
//! assert_eq!(chunks.len(), 2);
//!
//! // Gleicher Input, gleiche Digests.
//! assert_eq!(chunk_markdown(&source, text), chunks);
//!
//! // Vorschläge, keine geschriebenen Kanten.
//! let edges = suggest_relations(&chunks);
//! assert_eq!(edges.len(), 1);
//! ```
//!
//! # Stand
//! Knoten **AW3-05**; Ebene **L2** im Zielgraphen. Abhängigkeit
//! `harw-lens-types` (AW0-08, parallel gelandet): [`Chunk`](harw_lens_types::Chunk),
//! [`ChunkDigest`](harw_lens_types::ChunkDigest),
//! [`SourceRef`](harw_lens_types::SourceRef),
//! [`ByteSpan`](harw_lens_types::ByteSpan), [`EdgeKind`](harw_lens_types::EdgeKind).

mod markdown;
mod plain;
mod relations;
mod rust_code;
mod util;

pub use markdown::chunk_markdown;
pub use plain::{chunk_plain, DEFAULT_OVERLAP_BYTES, DEFAULT_TARGET_BYTES};
pub use relations::suggest_relations;
pub use rust_code::chunk_rust;
