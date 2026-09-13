//! Fehler-Enum für die Root-Space-Auflösung und das Scaffolding.
//!
//! Handgeschrieben nach Projektkonvention (kein `anyhow`/`thiserror`):
//! `Display` ist die einzige menschenlesbare Quelle, `Debug` delegiert an
//! `Display`, `std::error::Error::source` verlinkt die gewrappte Ursache, und
//! `From<std::io::Error>` erlaubt `?` an Dateisystem-Grenzen.

use std::fmt;
use std::path::PathBuf;

/// Kurzalias für Ergebnisse dieser Crate.
pub type HomeResult<T> = Result<T, HomeError>;

/// Alle Fehler, die beim Auflösen oder Anlegen des `~/.harw`-Root-Space
/// auftreten können.
pub enum HomeError {
    /// Weder `HARW_HOME` gesetzt noch ein Home-Verzeichnis auffindbar.
    NoHomeDirectory,
    /// `HARW_HOME` verweist auf einen Pfad, der existiert, aber kein
    /// Verzeichnis ist.
    HomeNotADirectory {
        /// Der aufgelöste, nicht-Verzeichnis-Pfad.
        path: PathBuf,
    },
    /// Ein Profilname enthält unzulässige Zeichen (nur `[A-Za-z0-9_-]+`).
    InvalidProfileName {
        /// Der abgelehnte Rohname.
        name: String,
    },
    /// Ein Sichtbarkeits-Indexname enthält unzulässige Zeichen (nur
    /// `[A-Za-z0-9_-]+`); insbesondere Pfadseparatoren und `..` sind
    /// verboten (Traversal-Schutz für [`crate::paths::visibility_index_dir`]).
    InvalidVisibilityName {
        /// Der abgelehnte Rohname.
        name: String,
    },
    /// Ein Dateisystem-Zugriff schlug fehl; `path` benennt das Ziel.
    Io {
        /// Pfad, an dem der I/O-Fehler auftrat.
        path: PathBuf,
        /// Die gewrappte Ursache.
        source: std::io::Error,
    },
}

impl fmt::Display for HomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHomeDirectory => write!(
                f,
                "could not resolve a home directory; set HARW_HOME to an existing directory"
            ),
            Self::HomeNotADirectory { path } => {
                write!(
                    f,
                    "HARW_HOME points at a non-directory path: {}",
                    path.display()
                )
            }
            Self::InvalidProfileName { name } => write!(
                f,
                "invalid profile name {name:?}; use only ASCII letters, digits, '-' or '_'"
            ),
            Self::InvalidVisibilityName { name } => write!(
                f,
                "invalid visibility name {name:?}; use only ASCII letters, digits, '-' or '_'"
            ),
            Self::Io { path, source } => {
                write!(f, "filesystem error at {}: {source}", path.display())
            }
        }
    }
}

impl fmt::Debug for HomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl std::error::Error for HomeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl HomeError {
    /// Hängt Pfadkontext an einen rohen `std::io::Error`, damit Aufrufer per
    /// `.map_err(|e| HomeError::io(path, e))?` propagieren können.
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
