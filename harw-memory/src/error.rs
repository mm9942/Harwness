//! Fehlertypen für `harw-memory`.
//!
//! # Verantwortungsbereich
//! Bündelt alle Fehler, die aus dem Memory-Backend nach außen dringen können.
//! Keine `anyhow`/`thiserror` — hand-geschriebenes Enum gemäß CLAUDE.md.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Ergebnistyp mit `MemoryError`.
pub type MemoryResult<T> = Result<T, MemoryError>;

/// Fehler des Memory-Subsystems.
///
/// # Beschreibung
/// Trägt genügend Kontext, um die Ursache ohne Sourcen-Blick zu verstehen.
/// `Debug` delegiert auf `Display` — eine Implementierung, keine Dopplung.
///
/// # Nebenläufigkeit
/// `Send + Sync + 'static`.
#[non_exhaustive]
pub enum MemoryError {
    /// I/O-Fehler beim Lesen/Schreiben einer Memory-Datei.
    Io {
        /// Betroffener Pfad.
        path: PathBuf,
        /// Ursache.
        source: io::Error,
    },
    /// Serialisierungs-/Deserialisierungsfehler in einem Signal-Eintrag.
    Serde {
        /// Kontext (welche Operation).
        context: &'static str,
        /// Ursache.
        source: serde_json::Error,
    },
    /// Ein Tier-Größenlimit wurde beim Schreiben überschritten.
    TierOverflow {
        /// Bezeichnung des Tiers (`"hot"`, `"warm/<namespace>"`).
        tier: String,
        /// Beobachtete Zeilenzahl.
        observed_lines: usize,
        /// Erlaubte Höchstzeilenzahl.
        limit_lines: usize,
    },
    /// Ein Namespace enthält ungültige Zeichen (Pfad-Traversal-Schutz).
    InvalidNamespace {
        /// Vom Aufrufer übergebener Namespace.
        namespace: String,
    },
    /// Wartungslauf konnte den Single-Writer-Lock nicht erwerben.
    LockContention {
        /// Beschreibung der versuchten Aktion.
        attempted: &'static str,
    },
    /// Frontmatter eines Fakts (`facts/<name>.md`) ist fehlerhaft oder
    /// unvollständig — z. B. fehlendes schließendes `---`, ein Pflichtfeld
    /// fehlt, oder ein Wert lässt sich nicht parsen.
    FrontmatterInvalid {
        /// Betroffene Datei.
        path: PathBuf,
        /// Menschenlesbare Ursache.
        reason: String,
    },
    /// Ein Fakt-Name verstößt gegen `[a-z0-9][a-z0-9-]{0,59}`
    /// (Pfad-Traversal-Schutz für `facts/<name>.md`).
    InvalidFactName {
        /// Vom Aufrufer übergebener Name.
        name: String,
    },
    /// Ein Frontmatter-Feld enthält einen Wert außerhalb der erlaubten
    /// Aufzählung (z. B. `type: unsinn`).
    InvalidEnumValue {
        /// Name des betroffenen Feldes (`"type"`, `"scope"`).
        field: &'static str,
        /// Der ungültige Rohwert.
        value: String,
    },
    /// Eine konfigurierte Fakt-Obergrenze (`[memory] max_facts` /
    /// `max_body_bytes`) wurde beim Schreiben überschritten; es wurde
    /// nichts geschrieben.
    LimitExceeded {
        /// Welche Grenze (`"max_facts"`, `"max_body_bytes"`).
        limit: &'static str,
        /// Konfigurierter Höchstwert.
        max: usize,
        /// Beobachteter bzw. durch den Schreibvorgang entstehender Wert.
        actual: usize,
    },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "memory I/O error at {}: {}", path.display(), source)
            }
            Self::Serde { context, source } => {
                write!(f, "memory serde error in {context}: {source}")
            }
            Self::TierOverflow {
                tier,
                observed_lines,
                limit_lines,
            } => write!(
                f,
                "memory tier {tier} overflow: {observed_lines} lines exceed limit {limit_lines}"
            ),
            Self::InvalidNamespace { namespace } => {
                write!(f, "memory namespace is invalid: {namespace:?}")
            }
            Self::LockContention { attempted } => {
                write!(f, "memory maintenance lock contended during {attempted}")
            }
            Self::FrontmatterInvalid { path, reason } => {
                write!(
                    f,
                    "fact frontmatter invalid at {}: {reason}",
                    path.display()
                )
            }
            Self::InvalidFactName { name } => {
                write!(f, "fact name is invalid: {name:?}")
            }
            Self::InvalidEnumValue { field, value } => {
                write!(f, "fact field {field:?} has invalid value: {value:?}")
            }
            Self::LimitExceeded { limit, max, actual } => {
                write!(f, "fact store limit {limit} exceeded: {actual} > {max}")
            }
        }
    }
}

impl fmt::Debug for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for MemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serde { source, .. } => Some(source),
            Self::TierOverflow { .. }
            | Self::InvalidNamespace { .. }
            | Self::LockContention { .. }
            | Self::FrontmatterInvalid { .. }
            | Self::InvalidFactName { .. }
            | Self::InvalidEnumValue { .. }
            | Self::LimitExceeded { .. } => None,
        }
    }
}
