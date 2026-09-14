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
    /// Der Trust-Store (`<home>/trusted-projects.toml`) ist unlesbar,
    /// fehlerhaft oder nicht serialisierbar (Format, Version, Duplikate,
    /// nicht-UTF-8-Pfad). Die Datei wird in diesem Fall nie stillschweigend
    /// als leer behandelt.
    TrustStore {
        /// Pfad der Trust-Store-Datei.
        path: PathBuf,
        /// Menschenlesbare Ursache.
        reason: String,
    },
    /// Ein Projekt kann nicht vertraut werden, weil sein `.harw` keine
    /// eindeutige, symlinkfreie Grundlage für den Digest bietet (Symlink,
    /// Sondertyp, Größen-/Anzahl-/Tiefengrenze).
    UntrustableProject {
        /// Betroffener Pfad (Projekt-`.harw` oder Eintrag darin).
        path: PathBuf,
        /// Menschenlesbare Ursache.
        reason: String,
    },
    /// Ein Projekt-Schlüssel (`project_key`) enthält unzulässige Zeichen (nur
    /// `[A-Za-z0-9_-]+`), z. B. beim Auflösen von
    /// [`crate::project::project_settings_dir`].
    InvalidProjectKey {
        /// Der abgelehnte Rohschlüssel.
        key: String,
    },
    /// Ein Projekt-Home (`<root>/.harw`) wurde für einen nicht unterstützten
    /// Root abgelehnt: das Dateisystem-Root (`/`) oder das Home-Verzeichnis
    /// des Benutzers selbst dürfen kein Projekt-Home bekommen
    /// ([`crate::project::ProjectHome::ensure`]).
    UnsupportedProjectHomeRoot {
        /// Der abgelehnte, kanonische Root.
        root: PathBuf,
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
            Self::TrustStore { path, reason } => {
                write!(f, "trust store {} is invalid: {reason}", path.display())
            }
            Self::UntrustableProject { path, reason } => write!(
                f,
                "project configuration at {} cannot be trusted: {reason}",
                path.display()
            ),
            Self::InvalidProjectKey { key } => write!(
                f,
                "invalid project key {key:?}; use only ASCII letters, digits, '-' or '_'"
            ),
            Self::UnsupportedProjectHomeRoot { root } => write!(
                f,
                "refusing to create a project home at {}: this is the filesystem root or the user's home directory",
                root.display()
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
