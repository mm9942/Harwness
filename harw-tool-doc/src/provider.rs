//! `DocToolProvider` — `ToolProvider` für `doc.read_pdf`.
//!
//! Spezifikationsquelle: `doc_read_pdf_design.md`, Abschnitt „Tool —
//! `harw-tool-doc/src/tool.rs` + `provider.rs`", Feld „Provider".
//!
//! # Verantwortung
//! Bündelt das einzige Tool dieses Crates (`doc.read_pdf`,
//! [`crate::tool::DocReadPdfExecutor`]) über `harw_tools::tool_provider!` zu
//! einem [`harw_extension_api::contributors::ToolProvider`] — dasselbe
//! Muster wie `WebToolProvider` in `harw-tool-web/src/provider.rs`. Das
//! Makro leitet `tools()`, `executor(name)` und `parallel_safe(name)` aus
//! den Consts und der `spec()`-Methode von [`crate::tool::DocReadPdfExecutor`]
//! ab (`NAME`, `PERMISSION`, `PARALLEL_SAFE`, `spec()`, `Default`).
//!
//! # Schlüsseltypen
//! - [`DocToolProvider`] — die generierte Provider-Struktur.
//!
//! # Nebenläufigkeit
//! [`DocToolProvider`] ist zustandslos und damit `Send + Sync + Copy`.
//! `doc.read_pdf` ist `parallel_safe` (reiner Lesezugriff).
//!
//! # Fehler
//! Der Provider selbst erzeugt keine Fehler; ein unbekannter Tool-Name
//! liefert `None` bzw. `false` (fail closed).
//!
//! # Examples
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_doc::DocToolProvider;
//!
//! let provider = DocToolProvider::new();
//! assert_eq!(provider.tools().len(), 1);
//! ```

use crate::tool::DocReadPdfExecutor;

harw_tools::tool_provider! {
    /// Stellt das Tool `doc.read_pdf` bereit.
    ///
    /// # Description
    /// Zustandslose Unit-Struktur; jeglicher Zustand — der prozessweite
    /// Mistral-OCR-Client — liegt in
    /// [`crate::mistral::install_mistral_ocr`]/[`crate::mistral::mistral_ocr`],
    /// nicht im Provider. Pfad- und Berechtigungs-Entscheidungen erfolgen
    /// per-Call aus dem [`harw_tools::executor::ToolExecutionContext`].
    ///
    /// # Concurrency
    /// `Send + Sync + Copy`; kein geteilter veränderlicher Zustand im Provider.
    pub struct DocToolProvider {
        DocReadPdfExecutor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{ToolName, ToolSpec};

    /// Der Provider bewirbt genau das eine Tool `doc.read_pdf`.
    #[test]
    fn test_doc_tool_provider_lists_one_tool() {
        assert_eq!(DocToolProvider::TOOL_NAMES, &["doc.read_pdf"]);

        let provider = DocToolProvider::new();
        let specs = provider.tools();
        let names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
        assert_eq!(names, vec!["doc.read_pdf"]);
    }

    /// Das beworbene Tool ist auch auflösbar.
    #[test]
    fn test_doc_tool_provider_resolves_advertised_tool() {
        let provider = DocToolProvider::new();
        for spec in provider.tools() {
            assert!(
                provider.executor(&ToolName::new(spec.name())).is_some(),
                "beworbenes Tool {:?} muss auflösbar sein",
                spec.name()
            );
        }
    }

    /// `doc.read_pdf` ist ein reiner Lesezugriff und damit parallelsicher.
    #[test]
    fn test_doc_tool_provider_is_parallel_safe() {
        let provider = DocToolProvider::new();
        assert!(provider.parallel_safe(&ToolName::new("doc.read_pdf")));
    }

    /// Unbekannte Namen fallen nicht auf ein Default-Tool zurück.
    #[test]
    fn test_doc_tool_provider_unknown_name_returns_none() {
        let provider = DocToolProvider::new();
        assert!(provider.executor(&ToolName::new("doc.unbekannt")).is_none());
        assert!(!provider.parallel_safe(&ToolName::new("doc.unbekannt")));
    }

    /// Audit: `doc.read_pdf` deklariert `ReadWorkspace` — ein ungeschütztes
    /// Datei-Tool wäre ein Sicherheitsloch.
    #[test]
    fn test_doc_tool_provider_declares_read_workspace_permission() {
        assert_eq!(
            DocToolProvider::TOOL_PERMISSIONS,
            &[Some(harw_tools::Permission::ReadWorkspace)]
        );
    }

    /// `new()` und `default()` liefern denselben zustandslosen Provider.
    #[test]
    fn test_doc_tool_provider_is_default_constructible() {
        assert_eq!(
            format!("{:?}", DocToolProvider::new()),
            format!("{:?}", DocToolProvider)
        );
    }
}
