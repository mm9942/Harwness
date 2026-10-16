//! Crate-weiter Fehlertyp für `harw-tool-fs`.
//!
//! # Verantwortung
//! Definiert [`FsToolError`] mit allen Varianten, die in den vier FS-Tools
//! auftreten können. Jede Variante trägt den vollständigen Diagnosekontext.
//!
//! # Schlüsseltypen
//! - [`FsToolError`]
//!
//! # Nebenläufigkeit
//! `FsToolError` ist `Send + Sync`.

use harw_macros::HarwError;

/// Crate-weiter Fehlertyp für alle Filesystem-Tools.
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zur Diagnose benötigt
/// wird, ohne Quellcode lesen zu müssen.
///
/// # Concurrency
/// `Send + Sync`; kein Mutex, kein Shared State.
#[derive(Debug, HarwError)]
pub enum FsToolError {
    /// `ReadWorkspace`-Permission fehlt im Sandbox-Kontext.
    #[msg("fs.read denied: ReadWorkspace permission missing")]
    ReadPermissionDenied,
    /// `WriteWorkspace`-Permission fehlt im Sandbox-Kontext.
    #[msg("fs.write denied: WriteWorkspace permission missing")]
    WritePermissionDenied,
    /// Der Pfad liegt außerhalb des erlaubten Workspace-Roots.
    #[msg("path '{path}' is outside the allowed workspace root")]
    PathOutsideRoot {
        /// Der verweigerte Pfad.
        path: String,
    },
    /// Datei oder Verzeichnis nicht gefunden.
    #[msg("file not found: '{path}'")]
    NotFound {
        /// Angeforderter Pfad.
        path: String,
    },
    /// Der Pfad ist kein reguläres File (z.B. Verzeichnis).
    #[msg("path is not a regular file: '{path}'")]
    NotAFile {
        /// Der Pfad, der kein File ist.
        path: String,
    },
    /// Der Pfad ist kein Verzeichnis.
    #[msg("path is not a directory: '{path}'")]
    NotADirectory {
        /// Der Pfad, der kein Verzeichnis ist.
        path: String,
    },
    /// Unterliegender I/O-Fehler.
    #[msg("I/O error: {0}")]
    #[from]
    Io(std::io::Error),
    /// Die JSON-Argumente sind ungültig oder unvollständig.
    #[msg("invalid arguments: {0}")]
    InvalidArgs(String),
    /// Die Datei überschreitet das konfigurierte Byte-Limit.
    #[msg("file is too large: {size} bytes exceeds limit of {max} bytes")]
    TooLarge {
        /// Tatsächliche Dateigröße in Bytes.
        size: u64,
        /// Konfiguriertes Maximum in Bytes.
        max: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::FsToolError;
    use std::error::Error as _;

    #[test]
    fn display_texts_are_stable() {
        assert_eq!(
            FsToolError::ReadPermissionDenied.to_string(),
            "fs.read denied: ReadWorkspace permission missing"
        );
        assert_eq!(
            FsToolError::NotFound {
                path: "a".to_owned()
            }
            .to_string(),
            "file not found: 'a'"
        );
        assert_eq!(
            FsToolError::TooLarge { size: 5, max: 3 }.to_string(),
            "file is too large: 5 bytes exceeds limit of 3 bytes"
        );
        assert_eq!(
            FsToolError::InvalidArgs("x".to_owned()).to_string(),
            "invalid arguments: x"
        );
    }

    #[test]
    fn io_wraps_with_source() {
        let err = FsToolError::from(std::io::Error::other("boom"));
        assert_eq!(err.to_string(), "I/O error: boom");
        assert!(err.source().is_some());
    }
}
