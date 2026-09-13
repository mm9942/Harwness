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

use std::fmt;

/// Crate-weiter Fehlertyp für alle Filesystem-Tools.
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zur Diagnose benötigt
/// wird, ohne Quellcode lesen zu müssen.
///
/// # Concurrency
/// `Send + Sync`; kein Mutex, kein Shared State.
#[derive(Debug)]
pub enum FsToolError {
    /// `ReadWorkspace`-Permission fehlt im Sandbox-Kontext.
    ReadPermissionDenied,
    /// `WriteWorkspace`-Permission fehlt im Sandbox-Kontext.
    WritePermissionDenied,
    /// Der Pfad liegt außerhalb des erlaubten Workspace-Roots.
    PathOutsideRoot {
        /// Der verweigerte Pfad.
        path: String,
    },
    /// Datei oder Verzeichnis nicht gefunden.
    NotFound {
        /// Angeforderter Pfad.
        path: String,
    },
    /// Der Pfad ist kein reguläres File (z.B. Verzeichnis).
    NotAFile {
        /// Der Pfad, der kein File ist.
        path: String,
    },
    /// Der Pfad ist kein Verzeichnis.
    NotADirectory {
        /// Der Pfad, der kein Verzeichnis ist.
        path: String,
    },
    /// Unterliegender I/O-Fehler.
    Io(std::io::Error),
    /// Die JSON-Argumente sind ungültig oder unvollständig.
    InvalidArgs(String),
    /// Die Datei überschreitet das konfigurierte Byte-Limit.
    TooLarge {
        /// Tatsächliche Dateigröße in Bytes.
        size: u64,
        /// Konfiguriertes Maximum in Bytes.
        max: u64,
    },
}

impl fmt::Display for FsToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadPermissionDenied => {
                write!(f, "fs.read denied: ReadWorkspace permission missing")
            }
            Self::WritePermissionDenied => {
                write!(f, "fs.write denied: WriteWorkspace permission missing")
            }
            Self::PathOutsideRoot { path } => {
                write!(f, "path '{path}' is outside the allowed workspace root")
            }
            Self::NotFound { path } => write!(f, "file not found: '{path}'"),
            Self::NotAFile { path } => write!(f, "path is not a regular file: '{path}'"),
            Self::NotADirectory { path } => {
                write!(f, "path is not a directory: '{path}'")
            }
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidArgs(reason) => write!(f, "invalid arguments: {reason}"),
            Self::TooLarge { size, max } => write!(
                f,
                "file is too large: {size} bytes exceeds limit of {max} bytes"
            ),
        }
    }
}

impl std::error::Error for FsToolError {
    /// Returns the underlying I/O error as source, if present.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for FsToolError {
    /// Wraps a standard I/O error into [`FsToolError::Io`].
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}
