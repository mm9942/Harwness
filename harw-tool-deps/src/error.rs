//! Crate-weiter Fehlertyp für `harw-tool-deps`.
//!
//! # Verantwortung
//! Definiert [`DepsToolError`] mit allen Fehlerfällen, die beim Lesen des
//! Workspace-Graphen (`graph_tool.rs`), des Lockfiles (`locked_tool.rs`) und
//! vor allem beim **Lesezugriff auf Dependency-Quellcode außerhalb des
//! Workspace** (`source_tool.rs`) auftreten können. Siehe AP W2-09..13.
//!
//! # Schlüsseltypen
//! - [`DepsToolError`] — das Fehler-Enum.
//! - `DepsToolResult<T>` — vom `HarwError`-Derive miterzeugter Typalias.
//!
//! # Sicherheitsrelevante Varianten
//! [`DepsToolError::PathEscapesRegistry`] ist die einzige Variante, die einen
//! abgewehrten Ausbruch aus der Registry-Wurzel meldet. Sie entsteht sowohl
//! bei lexikalischem `../`-Traversal (abgefangen von
//! `RegistrySourceLocator::resolve_contained`) als auch bei einem Symlink, der
//! erst nach `canonicalize()` aus der Registry herausführt.
//!
//! # Nebenläufigkeit
//! `DepsToolError` ist `Send + Sync`; es wird kein geteilter Zustand gehalten.

use harw_macros::HarwError;

/// Alle Fehlerfälle der Dependency-Werkzeuge.
///
/// # Description
/// `#[derive(HarwError)]` erzeugt `Display`, `std::error::Error` (inklusive
/// `source()` für die `#[from]`-Varianten), die zugehörigen `From`-Impls und
/// den Typalias `DepsToolResult<T>`.
///
/// Jede Variante trägt den vollständigen Diagnosekontext: ein Aufrufer muss
/// keinen Quellcode lesen, um zu verstehen, welcher Pfad, welches Crate oder
/// welches Limit die Ursache war.
///
/// # Concurrency
/// `Send + Sync`; kein Mutex, kein Shared State.
#[derive(Debug, HarwError)]
pub enum DepsToolError {
    /// Fehler aus `harw-code-graph`: Workspace-Manifest, `Cargo.lock` oder
    /// Registry-Auflösung. Trägt den ursprünglichen Fehler als `source()`.
    #[from]
    CodeGraph(harw_code_graph::CodeGraphError),

    /// Datei-I/O beim Lesen einer Quelldatei oder eines Verzeichnisses.
    #[from]
    Io(std::io::Error),

    /// Fehler beim Serialisieren einer JSON-Antwort.
    #[from]
    Json(serde_json::Error),

    /// Der aufgelöste Pfad liegt nach der Kanonisierung nicht mehr unterhalb
    /// von `$CARGO_HOME/registry/src` — abgewehrter `../`-Traversal oder ein
    /// aus der Registry herausführender Symlink.
    #[msg("Pfad '{path}' liegt außerhalb der Cargo-Registry-Wurzel und wurde abgelehnt")]
    PathEscapesRegistry {
        /// Der abgelehnte (bereits kanonisierte, sofern auflösbar) Pfad.
        path: String,
    },

    /// Für das angefragte Crate gibt es keinen `Cargo.lock`-Eintrag, und es
    /// wurde auch keine Version explizit übergeben.
    #[msg(
        "Crate '{crate_name}' ist in Cargo.lock nicht gesperrt; bitte 'version' explizit angeben"
    )]
    CrateNotLocked {
        /// Name des nicht gesperrten Crates.
        crate_name: String,
    },

    /// Die Datei überschreitet das für diesen Aufruf geltende Byte-Limit.
    #[msg("Datei '{path}' überschreitet das Limit von {limit} Bytes")]
    FileTooLarge {
        /// Pfad der zu großen Datei.
        path: String,
        /// Das wirksame Limit in Bytes (Aufruf-Limit, gedeckelt auf das harte Maximum).
        limit: u64,
    },

    /// Die Datei ist kein gültiges UTF-8. Es wird bewusst **nicht** verlustbehaftet
    /// dekodiert: Quellcode, den ein Agent zitiert, muss exakt sein.
    #[msg("Datei '{path}' ist kein gültiges UTF-8 und wird nicht verlustbehaftet dekodiert")]
    NotUtf8 {
        /// Pfad der nicht dekodierbaren Datei.
        path: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    /// `PathEscapesRegistry` nennt den abgelehnten Pfad im Klartext.
    #[test]
    fn test_display_path_escapes_registry_names_path() {
        let err = DepsToolError::PathEscapesRegistry {
            path: "/etc/passwd".to_owned(),
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("/etc/passwd"),
            "Meldung muss den Pfad nennen: {rendered}"
        );
    }

    /// `FileTooLarge` nennt Pfad und wirksames Limit.
    #[test]
    fn test_display_file_too_large_names_path_and_limit() {
        let err = DepsToolError::FileTooLarge {
            path: "src/lib.rs".to_owned(),
            limit: 262_144,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("src/lib.rs"), "Pfad fehlt: {rendered}");
        assert!(rendered.contains("262144"), "Limit fehlt: {rendered}");
    }

    /// `CrateNotLocked` nennt das Crate und den Ausweg.
    #[test]
    fn test_display_crate_not_locked_names_crate() {
        let err = DepsToolError::CrateNotLocked {
            crate_name: "serde".to_owned(),
        };
        assert!(err.to_string().contains("serde"));
    }

    /// `NotUtf8` nennt den Pfad.
    #[test]
    fn test_display_not_utf8_names_path() {
        let err = DepsToolError::NotUtf8 {
            path: "src/blob.bin".to_owned(),
        };
        assert!(err.to_string().contains("src/blob.bin"));
    }

    /// `?` konvertiert `std::io::Error` automatisch, und `source()` verweist
    /// auf den ursprünglichen Fehler.
    #[test]
    fn test_from_io_error_is_wrapped_and_exposed_as_source() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "keine Datei");
        let err: DepsToolError = io.into();
        assert!(matches!(err, DepsToolError::Io(_)));
        assert!(
            err.source().is_some(),
            "source() muss den I/O-Fehler zeigen"
        );
    }

    /// `CodeGraphError` wird über `#[from]` übernommen.
    #[test]
    fn test_from_code_graph_error_is_wrapped() {
        let inner = harw_code_graph::CodeGraphError::CargoHomeUnavailable;
        let err: DepsToolError = inner.into();
        assert!(matches!(err, DepsToolError::CodeGraph(_)));
        assert!(err.source().is_some());
    }

    /// Varianten ohne gewrappten Fremdfehler haben keine `source()`.
    #[test]
    fn test_source_is_none_for_structured_variants() {
        let err = DepsToolError::NotUtf8 {
            path: "x".to_owned(),
        };
        assert!(err.source().is_none());
    }

    /// Der vom Derive erzeugte Typalias ist nutzbar.
    #[test]
    fn test_result_alias_is_generated() {
        fn ok() -> DepsToolResult<u8> {
            Ok(7)
        }
        assert_eq!(ok().expect("Ok-Wert"), 7);
    }
}
