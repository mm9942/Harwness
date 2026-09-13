//! Bottom-up-Wissen über den eigenen Cargo-Workspace und über
//! Dependency-Quellen — Grundlage für den Analyse-Modus ("starte bei den
//! Leaf-Crates"). Siehe AP W1-14..17 für die vollständige Spezifikation.
//!
//! # Verantwortung
//! Dieses Crate liest ausschließlich TOML-Dateien (`Cargo.toml`,
//! `Cargo.lock`) und scannt lokale Verzeichnisse unter
//! `$CARGO_HOME/registry/src`. Es startet bewusst **keinen** `cargo`- oder
//! sonstigen Subprozess: read-only Sub-Agenten, die dieses Crate nutzen,
//! haben keine `ExecuteProcess`-Permission. Alles, was einen Subprozess
//! bräuchte (z. B. `cargo metadata`), ist außerhalb dieses Crates verortet.
//!
//! # Schlüsseltypen
//! - [`WorkspaceGraph`] / [`CrateNode`]: der Workspace-Abhängigkeitsgraph
//!   inklusive Ebenen-Berechnung (`workspace.rs`).
//! - [`LockedPackage`] / [`parse_lockfile`] / [`find_locked`]: minimaler
//!   `Cargo.lock`-Parser (`lockfile.rs`).
//! - [`RegistrySourceLocator`]: Auflösung lokaler Registry-Quellverzeichnisse
//!   sowie docs.rs-/crates.io-URL-Bau (`registry_locator.rs`).
//!
//! # Fehler
//! Alle fehlbaren Operationen liefern [`CodeGraphError`] (siehe `error.rs`).
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine Datenstrukturen bzw. zustandslose Leser (`Send +
//! Sync`); es werden keine Threads gestartet und kein globaler Zustand
//! gehalten.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_code_graph::WorkspaceGraph;
//! use std::path::Path;
//!
//! let graph = WorkspaceGraph::load(Path::new(".")).unwrap();
//! for level in graph.topological_levels().unwrap() {
//!     let names: Vec<&str> = level.iter().map(|c| c.name.as_str()).collect();
//!     println!("{names:?}");
//! }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod lockfile;
pub mod registry_locator;
pub mod workspace;

pub use error::{CodeGraphError, CodeGraphResult};
pub use lockfile::{LockedPackage, find_locked, parse_lockfile};
pub use registry_locator::RegistrySourceLocator;
pub use workspace::{CrateNode, WorkspaceGraph};
