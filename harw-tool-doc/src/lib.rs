//! `harw-tool-doc` — `doc.read_pdf`: seitenweises PDF-Lesen für den
//! Coding-Agent.
//!
//! Spezifikationsquelle: `doc_read_pdf_design.md` („Design: `doc.read_pdf`
//! (Crate `harw-tool-doc`)").
//!
//! # Zweck
//! Stellt ein Tool bereit:
//!
//! | Tool | Zweck |
//! |---|---|
//! | `doc.read_pdf` | PDF-Datei aus dem Workspace seitenweise als Text/Markdown lesen |
//!
//! Hauptweg ist Mistral OCR ([`mistral`]), wenn ein Mistral-Provider
//! konfiguriert ist (`harw-cli` ruft dafür einmalig beim Boot
//! [`install_mistral_ocr`] auf); sonst — und wenn der Mistral-Aufruf selbst
//! fehlschlägt — fällt das Tool auf lokale Textextraktion über
//! `oxidize-pdf` zurück ([`native`]).
//!
//! # Sicherheitskontrakt
//! 1. **Berechtigung vor Argumenten.** `doc.read_pdf` prüft
//!    [`harw_authority::Permission::ReadWorkspace`], bevor die vom Modell
//!    übergebenen Argumente verarbeitet werden.
//! 2. **Symlinkfestes Öffnen.** Die PDF-Datei wird — wie `fs.read` — über
//!    `harw_fsutil::open_beneath` relativ zur Workspace-Wurzel geöffnet;
//!    kein Pfadglied darf ein Symlink sein.
//! 3. **Egress nur über `harw-egress`.** Der Mistral-OCR-HTTP-Client
//!    entsteht ausschließlich über [`harw_egress::build_client`] mit einer
//!    auf den konfigurierten Host beschränkten [`harw_egress::EgressPolicy`].
//! 4. **Blockierendes im Blocking-Pool.** Datei-I/O und die native
//!    `oxidize-pdf`-Extraktion laufen über `tokio::task::spawn_blocking`,
//!    letztere zusätzlich mit einem Zeitbudget
//!    (`tool::DocReadPdfExecutor::execute`).
//! 5. **Panik-Isolation.** Jeder `oxidize-pdf`-Aufruf läuft in
//!    `std::panic::catch_unwind`; eine panikende Seite bricht nicht die
//!    gesamte Extraktion ab.
//!
//! # Schlüsseltypen
//! - [`DocToolProvider`] — der `ToolProvider` über `doc.read_pdf`.
//! - [`types::PageRange`], [`types::Backend`], [`types::ExtractedPage`],
//!   [`types::ExtractedDocument`] — der geteilte Vertrag zwischen `native`
//!   und `mistral`.
//! - [`DocToolError`] / [`DocToolResult`] — crate-weiter Fehlertyp.
//! - [`MistralOcrConfig`] / [`install_mistral_ocr`] — Mistral-OCR-Konfiguration.
//!
//! # Nebenläufigkeit
//! Alle öffentlichen Typen sind `Send + Sync`. Der Mistral-OCR-Client liegt
//! in einem prozessweiten `OnceLock<Arc<MistralOcrClient>>`
//! ([`install_mistral_ocr`] / `mistral::mistral_ocr`). `doc.read_pdf` ist
//! `parallel_safe`.
//!
//! # Fehler
//! Alle Fehler sind Varianten von [`DocToolError`]; an der Tool-Grenze werden
//! sie zu `Ok(ToolOutput::error(...))` — außer fehlerhaften JSON-Argumenten
//! (`harw_tools::ToolsError::InvalidArguments`).
//!
//! # Examples
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_doc::DocToolProvider;
//!
//! let provider = DocToolProvider::new();
//! for spec in provider.tools() {
//!     println!("{}", spec.name());
//! }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod mistral;
pub mod native;
pub mod provider;
pub mod tool;
pub mod types;

#[cfg(test)]
mod test_support;

pub use error::{DocToolError, DocToolResult};
pub use mistral::{DEFAULT_OCR_MODEL, MistralOcrConfig, install_mistral_ocr};
pub use provider::DocToolProvider;
pub use types::PageRange;
