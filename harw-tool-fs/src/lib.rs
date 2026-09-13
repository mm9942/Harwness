//! `harw-tool-fs` — Filesystem-Tool-Provider für den Harwness Coding-Agent.
//!
//! Stellt sechs Tools bereit (`fs.read`, `fs.write`, `fs.list`, `fs.search`,
//! `fs.glob`, `fs.grep`), die den ExtensionRegistry-fähigen
//! ToolProvider-Kontrakt aus `harw-extension-api` erfüllen. Alle Pfad- und
//! Berechtigungs-Entscheidungen erfolgen pro-Call aus dem
//! [`harw_tools::ToolExecutionContext`]. `fs.glob` und `fs.grep` sind über
//! `#[harw_macros::tool]` erzeugt (AP W2-01..03); die übrigen vier Tools sind
//! weiterhin handgeschriebene Executors (siehe `provider.rs` für die
//! Migrationsentscheidung).
//!
//! # Schlüsseltypen
//! - [`FsToolProvider`]: aggregierter Provider für alle sechs Tools.
//! - [`FsReadExecutor`], [`FsWriteExecutor`], [`FsListExecutor`], [`FsSearchExecutor`]:
//!   handgeschriebene Executors pro Tool.
//! - [`FsGlobTool`], [`FsGrepTool`]: makro-generierte Executors für `fs.glob`/`fs.grep`.
//! - [`FsToolError`]: crate-weiter Fehlertyp.
//!
//! # Sicherheit (W1-02)
//! Alle Pfade werden über [`harw_fsutil::open_beneath`] relativ zum
//! Deskriptor der Workspace-Wurzel geöffnet; kein Pfadglied darf ein Symlink
//! sein. Walks laufen über `harw_fsutil::walk_beneath` (interne Basis:
//! `tree`-Modul) mit harten Grenzen für Treffer, Tiefe, Einträge, Laufzeit,
//! Dateigröße und Ausgabemenge; der Abbruchgrund steht als `stopped` im
//! Ergebnis. `fs.write` schreibt atomar über `harw_fsutil::write_atomic` und
//! lehnt `.git/`- und `.harw/`-Pfade ab.
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`. Lesende Tools sind als `parallel_safe`
//! markiert. Walks und Datei-IO laufen in `tokio::task::spawn_blocking`.

#![forbid(unsafe_code)]

mod blocking;
pub mod error;
pub mod glob;
pub mod grep;
pub mod list;
pub mod provider;
pub mod read;
pub mod search;
#[cfg(test)]
mod test_support;
mod tree;
pub mod write;

pub use error::FsToolError;
pub use glob::{FsGlobTool, GlobArgs};
pub use grep::{FsGrepTool, GrepArgs};
pub use list::FsListExecutor;
pub use provider::FsToolProvider;
pub use read::FsReadExecutor;
pub use search::FsSearchExecutor;
pub use write::FsWriteExecutor;
